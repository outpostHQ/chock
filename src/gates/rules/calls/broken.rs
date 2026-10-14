//! The functions of one file and the calls each makes, read against the rules under `calls`.

use syn::visit::Visit;

use super::Rule;
use crate::gates::metrics::prodlines::parse_rust;
use crate::gates::rules::{Aliases, Callee, Pattern, is_test, line_of, production};

/// One call in a function: what it calls, the line it is made on, and where it ends.
struct Made {
    callee: Callee,
    line: u32,
    end: (usize, usize),
}

/// A function's name and the calls in its body, closures included.
struct Function {
    name: String,
    made: Vec<Made>,
}

/// Each rule a function breaks (nothing it needs ends before its first listed call or comes with
/// it), by its place in `calls`, with the function and that call's line. Tests are not read.
pub(super) fn broken(src: &str, rules: &[Rule]) -> Result<Vec<(usize, String, u32)>, String> {
    let file = parse_rust(src)?;
    let mut found = Functions {
        aliases: Aliases::of(&file),
        impls: Vec::new(),
        open: Vec::new(),
        done: Vec::new(),
    };
    for item in production(&file.items) {
        found.visit_item(item);
    }
    let mut broken = Vec::new();
    for Function { name, made } in &found.done {
        for (index, rule) in rules.iter().enumerate() {
            let Some(first) = made
                .iter()
                .filter(|call| rule.call.calls(&call.callee))
                .min_by_key(|call| call.end)
            else {
                continue;
            };
            let before = |pattern: &Pattern| {
                made.iter()
                    .any(|call| call.end < first.end && pattern.calls(&call.callee))
            };
            let with = |pattern: &Pattern| made.iter().any(|call| pattern.calls(&call.callee));
            if rule.after.as_ref().is_some_and(|after| !before(after))
                || rule.with.as_ref().is_some_and(|other| !with(other))
            {
                broken.push((index, name.clone(), first.line));
            }
        }
    }
    Ok(broken)
}

struct Functions {
    aliases: Aliases,
    /// The type of each `impl` block being read, innermost last, by the last segment of its path.
    impls: Vec<String>,
    /// The functions being read, innermost last: a function inside another is its own.
    open: Vec<Function>,
    done: Vec<Function>,
}

impl Functions {
    fn within(&mut self, name: String, body: impl FnOnce(&mut Self)) {
        self.open.push(Function {
            name,
            made: Vec::new(),
        });
        body(self);
        if let Some(function) = self.open.pop() {
            self.done.push(function);
        }
    }

    fn made(&mut self, callee: Callee, line: u32, end: proc_macro2::Span) {
        let end = end.end();
        if let Some(function) = self.open.last_mut() {
            function.made.push(Made {
                callee,
                line,
                end: (end.line, end.column),
            });
        }
    }
}

impl<'ast> Visit<'ast> for Functions {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let owner = match &*node.self_ty {
            syn::Type::Path(path) => path.path.segments.last().map(|last| last.ident.to_string()),
            _ => None,
        };
        self.impls.push(owner.unwrap_or_else(|| "impl".to_string()));
        syn::visit::visit_item_impl(self, node);
        self.impls.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.within(node.sig.ident.to_string(), |me| {
            syn::visit::visit_item_fn(me, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_test(&node.attrs) {
            return;
        }
        let owner = self.impls.last().map_or("impl", String::as_str);
        let name = format!("{owner}::{}", node.sig.ident);
        self.within(name, |me| syn::visit::visit_impl_item_fn(me, node));
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if is_test(&node.attrs) {
            return;
        }
        for item in &node.items {
            if let syn::TraitItem::Fn(function) = item
                && let Some(body) = &function.default
                && !is_test(&function.attrs)
            {
                let name = format!("{}::{}", node.ident, function.sig.ident);
                self.within(name, |me| me.visit_block(body));
            }
        }
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Some(callee) = Callee::of(node, &self.aliases) {
            let line = line_of(node.paren_token.span.open());
            self.made(callee, line, node.paren_token.span.close());
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let callee = Callee::Method(node.method.to_string());
        let line = line_of(node.method.span());
        self.made(callee, line, node.paren_token.span.close());
        syn::visit::visit_expr_method_call(self, node);
    }
}
