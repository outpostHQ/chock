//! Short names with a long life: a binding of one or two characters still read more than a few
//! lines below where it was bound, so its reader has to keep it in mind that far.

use proc_macro2::{Literal, Span, TokenStream, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::Visit;

/// The most lines below its binding where a short name may still be read.
pub(crate) const REACH: u32 = 5;

/// The longest name, in characters, that is short.
const SHORT: usize = 2;

/// Short names every Rust reader knows, wherever they are bound.
const KNOWN: [&str; 6] = ["tx", "rx", "cx", "io", "id", "ok"];

/// Short names a reader knows only as the binding of a `for` loop.
const COUNTERS: [&str; 3] = ["i", "j", "k"];

/// Where a pattern binds its names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    /// The pattern of a `for` loop, where `COUNTERS` are known names.
    ForLoop,
    /// Any other pattern.
    Elsewhere,
}

/// The line of each short binding last read more than `REACH` lines below it, within its scope.
/// A `let`, closure, `match` arm, `if let`, `while let` and `for` each bind.
#[must_use]
pub(crate) fn lines(file: &syn::File) -> Vec<u32> {
    let mut names = Names::default();
    names.visit_file(file);
    names
        .bindings
        .iter()
        .filter(|binding| binding.counts && binding.last.saturating_sub(binding.line) > REACH)
        .map(|binding| binding.line)
        .collect()
}

#[derive(Default)]
struct Names {
    /// The bindings of each open scope, innermost last; a later one shadows an earlier one.
    scopes: Vec<Vec<usize>>,
    bindings: Vec<Binding>,
    /// How many macro calls deep the walk is; there a string's `{name}` reads `name`.
    in_macro: usize,
}

struct Binding {
    name: String,
    line: u32,
    /// The last line that reads it, or its own line while nothing does.
    last: u32,
    /// False for a name every reader knows.
    counts: bool,
}

impl Names {
    fn scoped(&mut self, within: impl FnOnce(&mut Self)) {
        self.scopes.push(Vec::new());
        within(self);
        self.scopes.pop();
    }

    /// Brings each short name `pat` binds into the innermost scope. The cases of an or-pattern
    /// bind the same names, so a name binds once.
    fn bind(&mut self, pat: &syn::Pat, site: Site) {
        let mut idents = Vec::new();
        bound(pat, &mut idents);
        let mut seen: Vec<String> = Vec::new();
        for ident in idents {
            let name = unraw(ident);
            if !short(&name) || seen.contains(&name) {
                continue;
            }
            let known = KNOWN.contains(&name.as_str())
                || (site == Site::ForLoop && COUNTERS.contains(&name.as_str()));
            let line = line_of(ident.span());
            let index = self.bindings.len();
            self.bindings.push(Binding {
                name: name.clone(),
                line,
                last: line,
                counts: !known,
            });
            if let Some(scope) = self.scopes.last_mut() {
                scope.push(index);
            }
            seen.push(name);
        }
    }

    /// Marks the binding `name` resolves to here as read on `line`.
    fn read(&mut self, name: &str, line: u32) {
        let bindings = &self.bindings;
        let found = self
            .scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .copied()
            .find(|at| {
                bindings
                    .get(*at)
                    .is_some_and(|binding| binding.name == name)
            });
        if let Some(binding) = found.and_then(|at| self.bindings.get_mut(at)) {
            binding.last = binding.last.max(line);
        }
    }

    /// The condition of an `if` or a `while`: each `let` in it binds for the rest of the chain and
    /// for the body.
    fn conditions(&mut self, cond: &syn::Expr) {
        match cond {
            syn::Expr::Let(binding) => {
                self.visit_expr(&binding.expr);
                self.bind(&binding.pat, Site::Elsewhere);
            }
            syn::Expr::Binary(chain) if matches!(chain.op, syn::BinOp::And(_)) => {
                self.conditions(&chain.left);
                self.conditions(&chain.right);
            }
            other => self.visit_expr(other),
        }
    }

    /// A macro's arguments, walked as expressions where they parse as a list of them, and read
    /// name by name where they do not.
    fn arguments(&mut self, tokens: &TokenStream) {
        let list = Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
        self.in_macro = self.in_macro.saturating_add(1);
        match syn::parse::Parser::parse2(list, tokens.clone()) {
            Ok(exprs) => exprs.iter().for_each(|expr| self.visit_expr(expr)),
            Err(_) => self.idents(tokens),
        }
        self.in_macro = self.in_macro.saturating_sub(1);
    }

    fn idents(&mut self, tokens: &TokenStream) {
        for tree in tokens.clone() {
            match tree {
                TokenTree::Group(group) => self.idents(&group.stream()),
                TokenTree::Ident(ident) => self.read(&unraw(&ident), line_of(ident.span())),
                TokenTree::Literal(literal) => self.formatted(&literal),
                TokenTree::Punct(_) => {}
            }
        }
    }

    /// Each `{name}` a format string reads, on its own line.
    fn formatted(&mut self, token: &Literal) {
        let text = token.to_string();
        if !text.ends_with('"') {
            return;
        }
        let start = line_of(token.span());
        for (at, line) in (0_u32..).zip(text.split('\n')) {
            for name in inline(line) {
                self.read(name, start.saturating_add(at));
            }
        }
    }
}

impl<'ast> Visit<'ast> for Names {
    fn visit_item(&mut self, node: &'ast syn::Item) {
        // An item reads no local of the code around it.
        let outer = std::mem::take(&mut self.scopes);
        syn::visit::visit_item(self, node);
        self.scopes = outer;
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.scoped(|names| syn::visit::visit_block(names, node));
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.bind(&node.pat, Site::Elsewhere);
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.scoped(|names| {
            node.inputs
                .iter()
                .for_each(|input| names.bind(input, Site::Elsewhere));
            names.visit_expr(&node.body);
        });
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.visit_expr(&node.expr);
        self.scoped(|names| {
            names.bind(&node.pat, Site::ForLoop);
            names.visit_block(&node.body);
        });
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.scoped(|names| {
            names.conditions(&node.cond);
            names.visit_block(&node.then_branch);
        });
        if let Some((_, otherwise)) = &node.else_branch {
            self.visit_expr(otherwise);
        }
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.scoped(|names| {
            names.conditions(&node.cond);
            names.visit_block(&node.body);
        });
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.scoped(|names| {
            names.bind(&node.pat, Site::Elsewhere);
            if let syn::Pat::Guard(guarded) = &node.pat {
                names.conditions(&guarded.guard);
            }
            names.visit_expr(&node.body);
        });
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if let Some(ident) = node.path.get_ident().filter(|_| node.qself.is_none()) {
            self.read(&unraw(ident), line_of(ident.span()));
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.arguments(&node.tokens);
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        if self.in_macro > 0 {
            self.formatted(&node.token());
        }
    }
}

/// Every name `pat` binds, in the order written.
fn bound<'p>(pat: &'p syn::Pat, out: &mut Vec<&'p syn::Ident>) {
    match pat {
        syn::Pat::Ident(named) => {
            out.push(&named.ident);
            if let Some((_, inner)) = &named.subpat {
                bound(inner, out);
            }
        }
        syn::Pat::Guard(guarded) => bound(&guarded.pat, out),
        syn::Pat::Paren(inner) => bound(&inner.pat, out),
        syn::Pat::Reference(inner) => bound(&inner.pat, out),
        syn::Pat::Type(typed) => bound(&typed.pat, out),
        syn::Pat::Or(either) => either.cases.iter().for_each(|case| bound(case, out)),
        syn::Pat::Slice(slice) => slice.elems.iter().for_each(|elem| bound(elem, out)),
        syn::Pat::Tuple(tuple) => tuple.elems.iter().for_each(|elem| bound(elem, out)),
        syn::Pat::TupleStruct(tuple) => tuple.elems.iter().for_each(|elem| bound(elem, out)),
        syn::Pat::Struct(fields) => fields
            .fields
            .iter()
            .for_each(|field| bound(&field.pat, out)),
        // A constant, literal, macro, path, range, rest or wildcard binds nothing, and a shape a
        // later syn adds binds nothing chock can see.
        _ => {}
    }
}

/// One or two characters, not `_`-led and not capitalised, which is a constant or a variant.
fn short(name: &str) -> bool {
    let lower = name.chars().next().is_some_and(char::is_lowercase);
    lower && name.chars().count() <= SHORT
}

fn unraw(ident: &syn::Ident) -> String {
    let name = ident.to_string();
    name.strip_prefix("r#").map(str::to_string).unwrap_or(name)
}

fn line_of(span: Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

/// The names a line of a format string reads inline, as `{name}` or `{name:?}`. `{{` is a brace.
fn inline(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('{') {
        let after = rest.get(open.saturating_add(1)..).unwrap_or_default();
        if let Some(escaped) = after.strip_prefix('{') {
            rest = escaped;
            continue;
        }
        let end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = after.get(..end).unwrap_or_default();
        let closed = after
            .get(end..)
            .is_some_and(|tail| tail.starts_with(['}', ':']));
        if closed && !name.is_empty() {
            out.push(name);
        }
        rest = after;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str) -> Vec<u32> {
        lines(&syn::parse_file(source).unwrap())
    }

    /// Contract: `let v` last read 6 lines below counts, at its own line; 5 lines below does not.
    /// Mutant: `> REACH` as `>= REACH` counts the 5 too.
    #[test]
    fn a_short_name_read_six_lines_below_its_let_counts_and_five_lines_below_does_not() {
        let source = "fn far() {\n    let v = 1;\n    a();\n    a();\n    a();\n    a();\n    a();\n    b(v);\n}\n\
                      fn near() {\n    let v = 1;\n    a();\n    a();\n    a();\n    a();\n    b(v);\n}\n";
        assert_eq!(found(source), vec![2]);
    }

    #[test]
    fn known_names_a_counter_of_a_for_loop_and_a_leading_underscore_never_count() {
        let source = "fn f() {\n    let (tx, rx, cx, io, id, ok, _v) = g();\n    for i in 0..3 {\n        a();\n        a();\n        a();\n        a();\n        a();\n        b(i);\n    }\n    a();\n    a();\n    b((tx, rx, cx, io, id, ok, _v));\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }

    #[test]
    fn a_counter_name_bound_by_let_counts_as_any_short_name_does() {
        let source = "fn f() {\n    let i = 1;\n    a();\n    a();\n    a();\n    a();\n    a();\n    b(i);\n}\n";
        assert_eq!(found(source), vec![2]);
    }

    #[test]
    fn a_name_of_three_characters_or_a_capital_never_counts() {
        let source = "fn f() {\n    let abc = 1;\n    let N = 2;\n    a();\n    a();\n    a();\n    a();\n    a();\n    b(abc, N);\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }

    #[test]
    fn a_block_ends_the_scope_so_a_later_read_of_the_name_is_another_binding() {
        let source = "fn f() {\n    {\n        let v = 1;\n        b(v);\n    }\n    a();\n    a();\n    a();\n    a();\n    b(v);\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }

    #[test]
    fn shadowing_ends_the_old_binding_and_the_new_one_is_measured_from_its_own_line() {
        let source = "fn f() {\n    let v = 1;\n    let v = v + 1;\n    a();\n    a();\n    a();\n    a();\n    a();\n    a();\n    b(v);\n}\n";
        assert_eq!(found(source), vec![3]);
    }

    #[test]
    fn a_closure_a_match_arm_an_if_let_and_a_while_let_each_bind() {
        let gap = "        a();\n".repeat(5);
        let source = format!(
            "fn f() {{\n    let _ = |x| {{\n{gap}        b(x)\n    }};\n\
             match g() {{\n        Some(m) => {{\n{gap}        b(m)\n        }}\n        None => {{}}\n    }}\n\
             if let Some(n) = g() {{\n{gap}        b(n);\n    }}\n\
             while let Some(w) = g() {{\n{gap}        b(w);\n    }}\n}}\n"
        );
        assert_eq!(found(&source), vec![2, 11, 21, 29]);
    }

    /// Each `let` of a chain binds, a guard and a `let ... else` bind, and an `else` reads outside
    /// the `if`. Mutant: a chain read as one expression binds neither `p` nor `q`.
    #[test]
    fn a_let_chain_a_guard_and_a_let_else_each_bind() {
        let gap = "        a();\n".repeat(5);
        let source = [
            "fn f() {\n    let Some(e) = g() else {\n        return;\n    };\n",
            "    if let Some(p) = g() && let Some(q) = h(p) && ready() {\n",
            &gap,
            "        b(q);\n    } else {\n        b(e);\n    }\n",
            "    match g() {\n        Some(m) if m > 0 => {\n",
            &gap,
            "            b(m)\n        }\n        _ => {}\n    }\n}\n",
        ]
        .concat();
        assert_eq!(found(&source), vec![2, 5, 16]);
    }

    /// A `let` with no value binds, and so does a typed closure input. Mutant: a typed pattern
    /// that binds nothing loses `x`.
    #[test]
    fn a_let_with_no_value_and_a_typed_input_each_bind() {
        let gap = "        a();\n".repeat(5);
        let source = [
            "fn f() {\n    let v;\n    let _ = |x: u8| {\n",
            &gap,
            "        b(x)\n    };\n    b(v);\n}\n",
        ]
        .concat();
        assert_eq!(found(&source), vec![2, 3]);
    }

    /// Tokens that are no list of expressions are read one by one, into groups and strings, and
    /// `{{t}}` reads no `t`. Mutant: a reader that skips groups loses `w`.
    #[test]
    fn a_macro_that_holds_no_expressions_is_read_token_by_token() {
        let source = "fn f() {\n    let v = 1;\n    let w = 2;\n    let u = 3;\n    let t = 4;\n    \
                      a();\n    a();\n    a();\n    a();\n    a();\n    made!(v => (w) 1 \"{u}\");\n    \
                      println!(\"{{t}}\");\n}\n";
        assert_eq!(found(source), vec![2, 3, 4]);
    }

    #[test]
    fn a_read_inside_a_macro_or_its_format_string_counts_as_a_read() {
        let source = "fn f() {\n    let v = 1;\n    let w = 2;\n    a();\n    a();\n    a();\n    a();\n    a();\n    println!(\"{v}\");\n    made!(w => w);\n}\n";
        assert_eq!(found(source), vec![2, 3]);
    }

    #[test]
    fn each_shape_of_pattern_binds_each_name_written_in_it() {
        let written = "a @ Some(b) | (c, (k), &d, [e, ..], S { g, h: i }, T(j), 1, _)";
        let pat = syn::parse::Parser::parse_str(syn::Pat::parse_multi, written).unwrap();
        let mut names = Vec::new();
        bound(&pat, &mut names);
        let names: Vec<String> = names.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["a", "b", "c", "k", "d", "e", "g", "i", "j"]);
    }

    /// A guard or a type around a pattern binds the names inside it. Mutant: deleting either arm
    /// of `bound` binds nothing there.
    #[test]
    fn a_guarded_case_and_a_typed_input_bind_the_names_inside() {
        let names = |pat: &syn::Pat| {
            let mut out = Vec::new();
            bound(pat, &mut out);
            out.iter().map(ToString::to_string).collect::<Vec<String>>()
        };
        let arm: syn::Arm = syn::parse_str("Some(l) if l > 0 => {}").unwrap();
        assert_eq!(names(&arm.pat), ["l"]);
        let syn::FnArg::Typed(typed) = syn::parse_str::<syn::FnArg>("t: u8").unwrap() else {
            panic!("`t: u8` is a typed input");
        };
        assert_eq!(names(&syn::Pat::Type(typed)), ["t"]);
    }

    #[test]
    fn a_format_string_outside_a_macro_reads_no_name() {
        let source = "fn f() {\n    let v = 1;\n    a();\n    a();\n    a();\n    a();\n    a();\n    let _ = \"{v}\";\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }

    #[test]
    fn an_item_inside_a_function_reads_none_of_its_locals() {
        let source = "fn f() {\n    let v = 1;\n    b(v);\n    fn g() {\n        a();\n        a();\n        a();\n        a();\n        b(v);\n    }\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }
}
