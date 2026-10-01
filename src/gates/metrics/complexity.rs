//! Cognitive complexity per function, by Campbell's rules: how hard code is to follow, not how many
//! paths run through it.

use std::path::PathBuf;

use syn::visit::Visit;
use syn::{BinOp, Block, Expr, ExprBinary, ExprIf, Signature};

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

/// Score past which a function is hard to follow; splitting it into helpers only hides the number.
pub const HARD_TO_FOLLOW: u32 = 15;

/// Skipped beyond the tree-wide list: vendored code, fixtures, and targets that never ship.
const ALSO_SKIPPED: [&str; 5] = ["vendor", "fixtures", "tests", "examples", "benches"];

pub(crate) fn skipped(name: &str) -> bool {
    project::SKIPPED.contains(&name) || ALSO_SKIPPED.contains(&name)
}

pub const GATE: Gate = Gate {
    name: "complexity",
    about: "no function is harder to follow than the baseline records",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "cognitive",
    },
};

/// One function and its score. `line` is for navigation and stays out of the baseline key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub file: String,
    pub name: String,
    pub line: u32,
    pub score: u32,
}

/// Every function over the bar, keyed `file#name`; one that drops under it leaves the series.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut series = Series::new();
    let (paths, crates) = walked(ctx)?;
    for path in paths {
        let shown = project::relative(&ctx.root, &path);
        let source = std::fs::read_to_string(&path).map_err(|e| format!("reading {shown}: {e}"))?;
        let scored = match functions(&source, &shown) {
            Ok(found) => found,
            Err(_)
                if !project::is_crate_code(&shown, &crates)
                    || project::only_included(&ctx.root, &shown) =>
            {
                continue;
            }
            Err(why) => return Err(format!("{shown}:{why}")),
        };
        for found in scored {
            if found.score <= HARD_TO_FOLLOW {
                continue;
            }
            let key = format!("{}#{}", found.file, found.name);
            keep_worst(&mut series, &key, u64::from(found.score));
        }
    }
    Ok(series)
}

/// Keeps the higher value under `key`, since two functions in one file can share a name.
pub(crate) fn keep_worst(series: &mut Series, key: &str, value: u64) {
    let worst = series.get(key).unwrap_or(0).max(value);
    series.set(key, worst);
}

/// Every Rust file under the root, sorted, and the crate directories they sit in. A directory it
/// cannot read is an error, not a partial result.
fn walked(ctx: &Ctx) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let root = &ctx.root;
    let found = project::walked(&ctx.listing, root, &|name| !skipped(name), &|name, _| {
        name.ends_with(".rs") || name == project::MANIFEST
    })?;
    let crates = found
        .iter()
        .filter_map(|path| project::crate_dir(&project::relative(root, path)))
        .collect();
    let rust = found
        .into_iter()
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .collect();
    Ok((rust, crates))
}

/// Every named function in `source`, scored. `file` is only a label; an `Err` reads `line: reason`
/// for the caller to prefix with the path.
pub fn functions(source: &str, file: &str) -> Result<Vec<Function>, String> {
    let parsed =
        syn::parse_file(source).map_err(|e| format!("{}: {e}", line_of(e.span().start().line)))?;
    Ok(bodies(&parsed)
        .into_iter()
        .map(|found| Function {
            file: file.to_string(),
            score: score(&found),
            name: found.name,
            line: found.line,
        })
        .collect())
}

/// One function and its body. A function defined inside another is measured as part of its host.
pub struct Body<'ast> {
    pub name: String,
    pub line: u32,
    pub body: &'ast Block,
    /// Declared in an `impl` or a trait, where a bare call of its own name is some other function.
    pub method: bool,
}

/// Every function in a parsed file; `nesting` shares this list, so both gates see the same shapes.
#[must_use]
pub fn bodies(file: &syn::File) -> Vec<Body<'_>> {
    let mut collector = Collector { found: Vec::new() };
    collector.visit_file(file);
    collector.found
}

fn line_of(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

struct Collector<'ast> {
    found: Vec<Body<'ast>>,
}

impl<'ast> Collector<'ast> {
    fn take(&mut self, sig: &Signature, body: &'ast Block, method: bool) {
        self.found.push(Body {
            name: sig.ident.to_string(),
            line: line_of(sig.ident.span().start().line),
            body,
            method,
        });
    }
}

impl<'ast> Visit<'ast> for Collector<'ast> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.take(&node.sig, &node.block, false);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.take(&node.sig, &node.block, true);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(body) = &node.default {
            self.take(&node.sig, body, true);
        }
    }
}

fn score(found: &Body) -> u32 {
    let mut scorer = Scorer {
        name: found.name.clone(),
        method: found.method,
        ..Scorer::default()
    };
    scorer.visit_block(found.body);
    scorer.score + u32::from(scorer.recursive)
}

#[derive(Default)]
struct Scorer {
    score: u32,
    nesting: u32,
    /// The function being scored, so a call to itself is recognised as recursion.
    name: String,
    method: bool,
    recursive: bool,
}

impl Scorer {
    /// A structure costs one, plus one for each structure it sits inside.
    fn structure(&mut self) {
        self.score += 1 + self.nesting;
    }

    /// Walks what a structure owns one nesting level deeper.
    fn nested(&mut self, walk: impl FnOnce(&mut Self)) {
        self.nesting += 1;
        walk(self);
        self.nesting -= 1;
    }

    /// The `if` pays the nesting increment; each `else if` and `else` after it costs a flat one.
    fn chain(&mut self, node: &ExprIf, increment: u32) {
        self.score += 1 + increment;
        self.visit_expr(&node.cond);
        self.nested(|s| s.visit_block(&node.then_branch));
        match node.else_branch.as_ref().map(|(_, e)| &**e) {
            Some(Expr::If(next)) => self.chain(next, 0),
            Some(other) => {
                self.score += 1;
                self.nested(|s| s.visit_expr(other));
            }
            None => {}
        }
    }

    /// The operators of one boolean expression in source order. An operand that is not part of the
    /// run is visited as usual, so a parenthesised group becomes a run of its own.
    fn operators(&mut self, node: &ExprBinary, ops: &mut Vec<Logic>) {
        self.operand(&node.left, ops);
        ops.extend(logic(&node.op));
        self.operand(&node.right, ops);
    }

    fn operand(&mut self, expr: &Expr, ops: &mut Vec<Logic>) {
        match expr {
            Expr::Binary(inner) if logic(&inner.op).is_some() => self.operators(inner, ops),
            other => self.visit_expr(other),
        }
    }
}

impl<'ast> Visit<'ast> for Scorer {
    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.chain(node, self.nesting);
    }

    /// A `match` costs one however many arms it has.
    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        self.structure();
        self.visit_expr(&node.expr);
        self.nested(|s| {
            for arm in &node.arms {
                s.visit_arm(arm);
            }
        });
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.structure();
        self.nested(|s| s.visit_block(&node.body));
    }

    /// `while let` is a `while`: syn carries the pattern in the condition, so both forms land here.
    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.structure();
        self.visit_expr(&node.cond);
        self.nested(|s| s.visit_block(&node.body));
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.structure();
        self.visit_expr(&node.expr);
        self.nested(|s| s.visit_block(&node.body));
    }

    /// A run of `&&` or `||` costs one however long it is, and a change of operator starts another.
    /// A run takes no nesting increment.
    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if logic(&node.op).is_none() {
            syn::visit::visit_expr_binary(self, node);
            return;
        }
        let mut ops = Vec::new();
        self.operators(node, &mut ops);
        self.score += runs(&ops);
    }

    /// A labelled `break` or `continue` costs one; an unlabelled one costs nothing.
    fn visit_expr_break(&mut self, node: &'ast syn::ExprBreak) {
        self.score += u32::from(node.label.is_some());
        syn::visit::visit_expr_break(self, node);
    }

    fn visit_expr_continue(&mut self, node: &'ast syn::ExprContinue) {
        self.score += u32::from(node.label.is_some());
    }

    /// Recursion costs one in total. A free function recurses by its bare name; a method only
    /// through `Self::` or `self.`, since a bare name inside a method is another function.
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(called) = &*node.func {
            let names: Vec<String> = called
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            self.recursive |= match names.as_slice() {
                [only] => !self.method && *only == self.name,
                [owner, own] => self.method && owner == "Self" && *own == self.name,
                _ => false,
            };
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let on_self = matches!(&*node.receiver, Expr::Path(path) if path.path.is_ident("self"));
        self.recursive |= self.method && on_self && node.method == self.name;
        syn::visit::visit_expr_method_call(self, node);
    }

    /// A closure, or a function nested in a body, costs nothing itself but deepens what it holds.
    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.nested(|s| s.visit_expr(&node.body));
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.nested(|s| s.visit_block(&node.block));
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.nested(|s| s.visit_block(&node.block));
    }
}

/// The two operators cognitive complexity counts. `&` and `|` are arithmetic, not control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Logic {
    And,
    Or,
}

fn logic(op: &BinOp) -> Option<Logic> {
    match op {
        BinOp::And(_) => Some(Logic::And),
        BinOp::Or(_) => Some(Logic::Or),
        _ => None,
    }
}

fn runs(ops: &[Logic]) -> u32 {
    let changes = ops.windows(2).filter(|pair| pair[0] != pair[1]).count();
    u32::try_from(changes + usize::from(!ops.is_empty())).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Baseline;
    use std::path::Path;

    fn scored(source: &str) -> u32 {
        functions(source, "t.rs").unwrap().first().unwrap().score
    }

    fn names(source: &str) -> Vec<String> {
        functions(source, "t.rs")
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect()
    }

    /// A function of `count` flat `if`s, which scores exactly `count`.
    fn with_ifs(name: &str, count: usize) -> String {
        let body = "if a { g(); } ".repeat(count);
        format!("fn {name}(a: bool) {{ {body} }}")
    }

    fn tree(name: &str, files: &[(&str, &str)]) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        for (path, source) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, source).unwrap();
        }
        dir
    }

    #[test]
    fn a_file_cargo_never_compiles_that_the_parser_rejects_is_passed_over() {
        let root = tree(
            "complexity-corpus",
            &[
                ("Cargo.toml", ""),
                ("src/lib.rs", &with_ifs("deep", 20)),
                ("templates/c_macros.rs", "%%HEADER%%\nfn f() {}\n"),
            ],
        );
        assert_eq!(measured(&root).unwrap().get("src/lib.rs#deep"), Some(20));
    }

    fn measured(root: &Path) -> Result<Series, String> {
        measure(&Ctx::for_root(root.to_path_buf(), Baseline::empty("0.1.0")))
    }

    #[test]
    fn straight_line_code_scores_nothing_however_long_it_runs() {
        let source = "fn f() { let a = 1; let b = a + 2; let c = b * 3; println!(\"{c}\"); }";
        assert_eq!(scored(source), 0);
    }

    #[test]
    fn the_question_mark_and_an_early_return_cost_nothing() {
        let source = "fn f(a: Option<u8>) -> Option<u8> { let b = a?; return Some(b); }";
        assert_eq!(scored(source), 0);
    }

    #[test]
    fn a_single_if_costs_one_and_the_helper_scores_what_it_claims() {
        assert_eq!(scored(&with_ifs("f", 1)), 1);
        assert_eq!(scored(&with_ifs("f", 16)), 16);
    }

    #[test]
    fn a_nested_if_scores_higher_than_the_same_if_at_the_top_level() {
        let flat = "fn f(a: bool, b: bool) { if a { g(); } if b { h(); } }";
        let nested = "fn f(a: bool, b: bool) { if a { if b { h(); } } }";
        assert_eq!(scored(flat), 2);
        assert_eq!(scored(nested), 3);
    }

    #[test]
    fn each_level_of_nesting_costs_one_more_than_the_level_above_it() {
        let source = "fn f(a: bool) { for i in a { while a { if a { g(); } } } }";
        assert_eq!(scored(source), 6);
    }

    #[test]
    fn an_else_if_chain_costs_one_each_and_takes_no_nesting_increment() {
        let source = "fn f(a: bool, b: bool) { if a { g(); } else if b { h(); } else { i(); } }";
        assert_eq!(scored(source), 3);
    }

    #[test]
    fn an_else_inside_a_loop_still_costs_only_one_while_its_if_pays_the_level() {
        let source = "fn f(a: bool) { for i in a { if a { g(); } else { h(); } } }";
        assert_eq!(scored(source), 4);
    }

    #[test]
    fn a_run_of_the_same_operator_counts_once_however_long_it_is() {
        let source = "fn f(a: bool) { if a && a && a && a { g(); } }";
        assert_eq!(scored(source), 2);
    }

    #[test]
    fn a_change_of_operator_starts_a_new_run() {
        let source = "fn f(a: bool, b: bool) { if a && b || a || b && a { g(); } }";
        assert_eq!(scored(source), 4);
    }

    #[test]
    fn a_parenthesised_group_is_a_run_of_its_own() {
        let source = "fn f(a: bool, b: bool) { if a && (b || a) && b { g(); } }";
        assert_eq!(scored(source), 3);
    }

    #[test]
    fn a_boolean_run_takes_no_nesting_increment_of_its_own() {
        let source = "fn f(a: bool) { if a { if a && a { g(); } } }";
        assert_eq!(scored(source), 4);
    }

    #[test]
    fn a_match_costs_one_however_many_arms_it_has() {
        let source = "fn f(n: u8) -> u8 { match n { 0 => 1, 1 => 2, 2 => 3, _ => 0 } }";
        assert_eq!(scored(source), 1);
    }

    #[test]
    fn a_match_inside_a_loop_pays_for_the_level_it_sits_at() {
        let source = "fn f(a: bool) { for n in a { match n { 0 => g(), _ => h() } } }";
        assert_eq!(scored(source), 3);
    }

    #[test]
    fn every_loop_form_costs_the_same() {
        assert_eq!(scored("fn f(a: bool) { for i in a { g(); } }"), 1);
        assert_eq!(scored("fn f(a: bool) { while a { g(); } }"), 1);
        assert_eq!(scored("fn f() { loop { g(); } }"), 1);
        assert_eq!(
            scored("fn f(a: Option<u8>) { while let Some(b) = a { g(b); } }"),
            1
        );
    }

    #[test]
    fn if_let_is_an_if_like_any_other() {
        let source = "fn f(a: Option<u8>) { if let Some(b) = a { g(b); } else { h(); } }";
        assert_eq!(scored(source), 2);
    }

    #[test]
    fn a_closure_deepens_the_nesting_for_what_is_inside_it() {
        let outside = "fn f(a: bool) { if a { g(); } }";
        let inside = "fn f(a: bool) { let c = |x: bool| { if x { g(); } }; c(a); }";
        assert_eq!(scored(outside), 1);
        assert_eq!(scored(inside), 2);
    }

    #[test]
    fn a_function_defined_inside_another_is_scored_into_its_host() {
        let source = "fn f(a: bool) { fn g(b: bool) { if b { h(); } } g(a); }";
        assert_eq!(names(source), vec!["f".to_string()]);
        assert_eq!(scored(source), 2);
    }

    #[test]
    fn a_method_is_found_by_its_own_name_and_a_trait_method_needs_a_body() {
        let source = "struct S; impl S { fn m(&self, a: bool) { if a { g(); } } }";
        assert_eq!(names(source), vec!["m".to_string()]);
        assert_eq!(scored(source), 1);
        let tr = "trait T { fn bare(&self); fn full(&self, a: bool) { if a { g(); } } }";
        assert_eq!(names(tr), vec!["full".to_string()]);
    }

    #[test]
    fn a_function_records_the_line_its_name_is_on() {
        let source = "\n\n// a comment\nfn f() {}\n";
        assert_eq!(functions(source, "t.rs").unwrap().first().unwrap().line, 4);
    }

    #[test]
    fn a_tangled_function_scores_what_its_parts_add_up_to() {
        let source = "
fn f(a: bool, b: Option<u8>) {
    for x in a {
        if a && b {
            match x {
                0 => g(),
                _ => {
                    while a {
                        h();
                    }
                }
            }
        } else {
            i();
        }
    }
}";
        assert_eq!(scored(source), 12);
    }

    #[test]
    fn a_jump_to_a_label_costs_one_and_an_unlabelled_one_costs_nothing() {
        assert_eq!(scored("fn f() { loop { break; } }"), 1);
        assert_eq!(scored("fn f() { 'outer: loop { break 'outer; } }"), 2);
        assert_eq!(
            scored("fn f(v: &[u8]) { 'outer: for x in v { if *x == 0 { continue 'outer; } } }"),
            4
        );
    }

    #[test]
    fn recursion_costs_one_however_many_times_the_function_calls_itself() {
        assert_eq!(
            scored("fn f(n: u32) -> u32 { if n == 0 { 0 } else { f(n - 1) + f(n - 1) } }"),
            3
        );
        assert_eq!(scored("fn g(n: u32) -> u32 { h(n) }"), 0);
    }

    #[test]
    fn a_method_recurses_through_self_and_a_bare_call_of_its_name_is_another_function() {
        assert_eq!(scored("impl S { fn walk(&self) { self.walk() } }"), 1);
        assert_eq!(scored("impl S { fn build() { Self::build() } }"), 1);
        assert_eq!(scored("impl S { fn parse(&self) { parse(1) } }"), 0);
        assert_eq!(
            scored("impl S { fn walk(&self, o: &S) { o.walk_other() } }"),
            0
        );
    }

    #[test]
    fn a_longer_path_or_a_call_through_a_value_is_not_recursion() {
        assert_eq!(scored("fn f() { crate::m::f() }"), 0);
        assert_eq!(scored("fn f(g: fn()) { (g)() }"), 0);
        assert_eq!(
            scored("impl S { fn walk(&self) { crate::S::walk(self) } }"),
            0
        );
    }

    #[test]
    fn only_a_function_over_the_bar_reaches_the_series() {
        let source = format!("{}\n{}", with_ifs("over", 16), with_ifs("under", 15));
        let dir = tree("complexity-bar", &[("src/a.rs", source.as_str())]);
        let series = measured(&dir).unwrap();
        assert_eq!(series.get("src/a.rs#over"), Some(16));
        assert_eq!(series.get("src/a.rs#under"), None);
        assert_eq!(series.0.keys().collect::<Vec<_>>(), vec!["src/a.rs#over"]);
    }

    #[test]
    fn a_function_under_the_bar_leaves_the_ones_after_it_to_be_measured() {
        let source = format!("{}\n{}", with_ifs("quiet", 15), with_ifs("loud", 17));
        let dir = tree("complexity-after-quiet", &[("src/a.rs", source.as_str())]);
        let series = measured(&dir).unwrap();
        assert_eq!(series.get("src/a.rs#loud"), Some(17));
        assert_eq!(series.get("src/a.rs#quiet"), None);
    }

    #[test]
    fn two_functions_of_the_same_name_in_one_file_keep_the_worst() {
        let source = format!("{}\n{}", with_ifs("parse", 18), with_ifs("parse", 25));
        let dir = tree("complexity-duplicate", &[("src/a.rs", source.as_str())]);
        assert_eq!(measured(&dir).unwrap().get("src/a.rs#parse"), Some(25));
    }

    #[test]
    fn a_file_that_does_not_parse_is_a_failure_to_run_rather_than_a_zero() {
        // The manifest matters: only a file some crate compiles stops the gate.
        let dir = tree(
            "complexity-broken",
            &[("Cargo.toml", ""), ("src/a.rs", "fn f( { this is not rust")],
        );
        let err = measured(&dir).unwrap_err();
        assert!(err.starts_with("src/a.rs:1:"), "{err}");
    }

    #[test]
    fn a_source_file_under_a_skipped_directory_is_not_measured() {
        let over = with_ifs("over", 20);
        let dir = tree(
            "complexity-skipped",
            &[
                ("src/a.rs", over.as_str()),
                ("target/debug/build/a.rs", over.as_str()),
                ("tests/fixtures/a.rs", over.as_str()),
            ],
        );
        let series = measured(&dir).unwrap();
        assert_eq!(series.0.keys().collect::<Vec<_>>(), vec!["src/a.rs#over"]);
    }

    /// A directory lists in no fixed order, so a skipped name can come before files still to walk.
    #[test]
    fn every_skipped_name_is_passed_over_without_ending_the_listing_it_sits_in() {
        let loud = with_ifs("loud", 17);
        let mut paths: Vec<String> = project::SKIPPED
            .iter()
            .chain(ALSO_SKIPPED.iter())
            .map(|d| format!("{d}/hidden.rs"))
            .collect();
        paths.extend(["main.rs", "lib.rs", "b.rs"].map(String::from));
        let files: Vec<(&str, &str)> = paths.iter().map(|p| (p.as_str(), loud.as_str())).collect();
        let dir = tree("complexity-skip-order", &files);
        let series = measured(&dir).unwrap();
        assert_eq!(
            series.0.keys().collect::<Vec<_>>(),
            vec!["b.rs#loud", "lib.rs#loud", "main.rs#loud"]
        );
    }

    #[test]
    fn a_tree_with_no_function_over_the_bar_measures_empty_rather_than_failing() {
        let dir = tree("complexity-clean", &[("src/a.rs", "fn f() {}")]);
        assert_eq!(measured(&dir).unwrap(), Series::new());
    }

    #[test]
    fn the_gate_is_a_ratchet_over_items_counted_in_cognitive_points() {
        assert_eq!(GATE.name, "complexity");
        assert_eq!(crate::run::rerun(GATE.name), "chock run complexity");
        match GATE.kind {
            Kind::Ratchet { keys, unit, .. } | Kind::AnnotatedRatchet { keys, unit, .. } => {
                assert_eq!(keys, Keys::Items);
                assert_eq!(unit, "cognitive");
            }
            Kind::Binary(_) | Kind::Debt { .. } => {
                panic!("complexity is a ratchet, not a pass/fail gate")
            }
        }
    }
}
