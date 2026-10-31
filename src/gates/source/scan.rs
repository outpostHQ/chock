//! The scan of one source file: its lint suppressions, their reasons, and the calls a test-only item takes.

use std::fmt::Write as _;

use super::interpolation::{dynamic, is_true, only, text};
use super::testonly::test_gate;
use super::{
    COMMAND_NEW, CRATE_LEVEL, DISABLED_TLS, DYNAMIC_SHELL, Delivery, Imports, LEAST_REASON,
    Reading, SHELLS, Seen, TLS_BUILDERS, UNREASONED, named, reasoned, shipped, stack,
};
use crate::project::release::{LINT_REASONS, Release};
use crate::run::report::Finding;
use proc_macro2::LineColumn;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{AttrStyle, Attribute, Expr, ExprLit, Lit, Meta, Token};

pub(super) struct Scan<'a> {
    pub(super) path: &'a str,
    pub(super) src: &'a str,
    starts: Vec<usize>,
    imports: Imports,
    pub(super) attrs: Vec<Seen>,
    /// How many `#[cfg(test)]` items or `#[test]` functions enclose the node being visited.
    tests: usize,
    pub(super) test_file: bool,
    /// Whether anybody receives this file; see `judged`.
    ships: bool,
    pub(super) test_only: TestOnly<'a>,
    /// The `rust-version` of the file's crate, which decides how an `allow` gives its reason.
    pub(super) promise: Option<Release>,
    findings: Vec<Finding>,
}

/// Whether a named feature is one no shipped build enables, read from the workspace manifests.
type TestOnly<'a> = &'a dyn Fn(&str) -> bool;

impl<'a> Scan<'a> {
    pub(super) fn new(
        path: &'a str,
        src: &'a str,
        imports: Imports,
        delivery: Delivery,
        only: TestOnly<'a>,
    ) -> Self {
        Self {
            path,
            src,
            starts: std::iter::once(0)
                .chain(src.match_indices('\n').map(|(index, _)| index + 1))
                .collect(),
            imports,
            attrs: Vec::new(),
            tests: 0,
            test_file: test_path(path),
            ships: delivery == Delivery::Shipped,
            test_only: only,
            promise: None,
            findings: Vec::new(),
        }
    }

    pub(super) fn into_findings(mut self) -> Vec<Finding> {
        self.attrs.sort_by_key(|seen| seen.from);
        let mut found = self.findings;
        found.extend(unreasoned(self.path, self.src, &self.attrs, self.promise));
        if self.ships {
            found.extend(shipped(self.path, &self.attrs));
        }
        found.extend(stacked(self.path, self.src, &self.attrs));
        Finding::sort_by_place(&mut found);
        found
    }

    /// Reports an inner `#![allow(...)]` at a crate root, which covers the whole crate. In a module
    /// file only the reason rule applies.
    fn blanket(&mut self, attrs: &[Attribute], scope: &str) {
        // A test target has no scope narrower than its own file to ask for.
        if self.in_tests() || !is_crate_root(self.path) {
            return;
        }
        for attr in attrs.iter().filter(|a| is_inner(a)) {
            let Some(reading) = allow_reading(attr) else {
                continue;
            };
            let message = format!("#![allow({})] covers {scope}", reading.lints.join(", "));
            self.findings.push(
                Finding::at(self.path, &message)
                    .line(line_of(attr.pound_token.span.start()))
                    .item(CRATE_LEVEL),
            );
        }
    }

    fn record(&mut self, attr: &Attribute) {
        self.attrs.push(Seen {
            inner: is_inner(attr),
            from: offset(self.src, &self.starts, attr.pound_token.span.start()),
            to: offset(
                self.src,
                &self.starts,
                attr.bracket_token.span.close().end(),
            ),
            line: line_of(attr.pound_token.span.start()),
            allow: allow_reading(attr),
            suppresses: suppressed_lints(attr),
            in_tests: self.in_tests(),
        });
    }

    pub(super) fn in_tests(&self) -> bool {
        self.tests > 0 || self.test_file
    }

    pub(super) fn report(&mut self, at: LineColumn, rule: &str, message: &str) {
        let line = line_of(at);
        // A deliberate shape has no attribute to carry a reason, so a comment naming the rule and
        // saying why waives it.
        if waived(self.src, line, rule) {
            return;
        }
        self.findings
            .push(Finding::at(self.path, message).line(line).item(rule));
    }

    /// reqwest's builder told to accept an unverified certificate or hostname. Test code is exempt:
    /// a local test server's certificate is usually self-signed.
    fn tls(&mut self, call: &syn::ExprMethodCall) {
        let message = match call.method.to_string().as_str() {
            "tls_danger_accept_invalid_certs" | "danger_accept_invalid_certs" => {
                "reqwest client builder disables TLS certificate verification"
            }
            "tls_danger_accept_invalid_hostnames" | "danger_accept_invalid_hostnames" => {
                "reqwest client builder disables TLS hostname verification"
            }
            _ => return,
        };
        if self.in_tests() || !only(&call.args).is_some_and(is_true) {
            return;
        }
        let mut base = &*call.receiver;
        while let Expr::MethodCall(inner) = base {
            base = &*inner.receiver;
        }
        let Expr::Call(builder) = base else {
            return;
        };
        if !builder.args.is_empty()
            || !TLS_BUILDERS
                .iter()
                .any(|item| calls(&builder.func, &self.imports, item))
        {
            return;
        }
        self.report(call.method.span().start(), DISABLED_TLS, message);
    }

    /// A value built at run time handed to a shell as its `-c` script. Test code is not exempt.
    fn shell(&mut self, call: &syn::ExprMethodCall) {
        if call.method != "arg" || !only(&call.args).is_some_and(dynamic) {
            return;
        }
        let Expr::MethodCall(script) = &*call.receiver else {
            return;
        };
        if script.method != "arg" || only(&script.args).and_then(text).as_deref() != Some("-c") {
            return;
        }
        let Expr::Call(command) = &*script.receiver else {
            return;
        };
        if !calls(&command.func, &self.imports, COMMAND_NEW) {
            return;
        }
        if !only(&command.args)
            .and_then(text)
            .is_some_and(|shell| SHELLS.contains(&shell.as_str()))
        {
            return;
        }
        let message = "a dynamic value is interpolated into a shell command";
        self.report(call.method.span().start(), DYNAMIC_SHELL, message);
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_file(&mut self, node: &'ast syn::File) {
        self.blanket(&node.attrs, "this whole file");
        syn::visit::visit_file(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let gated = test_gate(&node.attrs, self.test_only);
        self.tests += usize::from(gated);
        self.blanket(&node.attrs, &format!("the whole module `{}`", node.ident));
        syn::visit::visit_item_mod(self, node);
        self.tests -= usize::from(gated);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let gated = test_gate(&node.attrs, self.test_only);
        self.tests += usize::from(gated);
        syn::visit::visit_item_fn(self, node);
        self.tests -= usize::from(gated);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let gated = test_gate(&node.attrs, self.test_only);
        self.tests += usize::from(gated);
        syn::visit::visit_impl_item_fn(self, node);
        self.tests -= usize::from(gated);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        self.record(node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.tls(node);
        self.shell(node);
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Every `#[allow]` that switches a lint off without a `reason`. A crate whose `rust-version`
/// cannot build `reason = "…"` gives the reason in a `//` comment beside the attribute instead.
pub(super) fn unreasoned(
    path: &str,
    src: &str,
    attrs: &[Seen],
    promise: Option<Release>,
) -> Vec<Finding> {
    let below = promise.filter(|promise| !LINT_REASONS.open_to(Some(*promise)));
    attrs
        .iter()
        .filter_map(|seen| {
            let reading = seen.allow.as_ref().filter(|reading| !reading.reasoned)?;
            if below.is_some() && commented(src, seen) {
                return None;
            }
            let bang = if seen.inner { "!" } else { "" };
            let lints = reading.lints.join(", ");
            let mut message =
                format!("#{bang}[allow({lints})] switches a lint off with no stated reason");
            if let Some(promise) = below {
                let _ = write!(
                    message,
                    "; rust-version {promise} cannot build `reason = \"…\"`, so write the reason in a \
                     `//` comment on the line above"
                );
            }
            Some(Finding::at(path, &message).line(seen.line).item(UNREASONED))
        })
        .collect()
}

/// Whether a `//` comment of `LEAST_REASON` characters or more fills the line above the attribute
/// or follows it on its last line. A doc comment documents the item, so it is no reason.
fn commented(src: &str, seen: &Seen) -> bool {
    let above = src
        .get(..seen.from)
        .and_then(|before| before.rsplit('\n').nth(1));
    let after = src.get(seen.to..).and_then(|rest| rest.split('\n').next());
    [above, after].into_iter().flatten().any(|line| {
        let text = line.trim_start().strip_prefix("//");
        text.filter(|rest| !rest.starts_with(['/', '!']))
            .is_some_and(|rest| rest.trim().chars().count() >= LEAST_REASON)
    })
}

/// Items carrying two or more `#[allow]` attributes, or one naming four or more lints.
fn stacked(path: &str, src: &str, attrs: &[Seen]) -> Vec<Finding> {
    runs(src, attrs)
        .into_iter()
        .filter_map(|run| stack(path, run))
        .collect()
}

/// Groups of attributes separated only by whitespace and comments: those on the same item.
pub(super) fn runs<'a>(src: &str, attrs: &'a [Seen]) -> Vec<&'a [Seen]> {
    let mut out = Vec::new();
    let mut start = 0;
    for index in 1..attrs.len() {
        let joined = src
            .get(attrs[index - 1].to..attrs[index].from)
            .is_some_and(only_trivia);
        if joined {
            continue;
        }
        out.push(&attrs[start..index]);
        start = index;
    }
    if start < attrs.len() {
        out.push(&attrs[start..]);
    }
    out
}

/// Whether the text is only whitespace and comments. A doc comment is an attribute, so it is never
/// in a gap.
fn only_trivia(text: &str) -> bool {
    let mut rest = text.trim_start();
    loop {
        if let Some(tail) = rest.strip_prefix("//") {
            rest = tail
                .split_once('\n')
                .map_or("", |(_, after)| after)
                .trim_start();
            continue;
        }
        let Some(tail) = rest.strip_prefix("/*") else {
            return rest.is_empty();
        };
        let Some(after) = past_block(tail) else {
            return false;
        };
        rest = after.trim_start();
    }
}

/// Past the `*/` closing a block comment, counting nesting the way rustc counts it.
fn past_block(mut rest: &str) -> Option<&str> {
    let mut depth = 1usize;
    while depth > 0 {
        let close = rest.find("*/")?;
        match rest.find("/*") {
            Some(open) if open < close => {
                depth += 1;
                rest = rest.get(open + 2..)?;
            }
            _ => {
                depth -= 1;
                rest = rest.get(close + 2..)?;
            }
        }
    }
    Some(rest)
}

/// What a plain `allow(...)` names. `expect` is skipped since it fails once the lint stops firing,
/// and `cfg_attr` is left to `suppressed_lints`.
fn allow_reading(attr: &Attribute) -> Option<Reading> {
    let Meta::List(list) = &attr.meta else {
        return None;
    };
    if !list.path.is_ident("allow") {
        return None;
    }
    let (lints, reasoned) = listed(list)?;
    (!lints.is_empty()).then_some(Reading { lints, reasoned })
}

/// Every lint this attribute switches off outside a test build: `allow`, `expect`, or either inside
/// a `cfg_attr` whose predicate is not `test`.
fn suppressed_lints(attr: &Attribute) -> Vec<String> {
    let Meta::List(list) = &attr.meta else {
        return Vec::new();
    };
    if switches_off(&list.path) {
        return lint_names(list);
    }
    if !list.path.is_ident("cfg_attr") {
        return Vec::new();
    }
    let Ok(args) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
        return Vec::new();
    };
    let mut reading = args.into_iter();
    // Only `cfg_attr(test, …)` is test-only; any other predicate can reach a shipped build.
    match reading.next() {
        Some(Meta::Path(pred)) if pred.is_ident("test") => return Vec::new(),
        Some(_) => {}
        None => return Vec::new(),
    }
    reading
        .filter_map(|inner| match inner {
            Meta::List(inner) if switches_off(&inner.path) => Some(lint_names(&inner)),
            _ => None,
        })
        .flatten()
        .collect()
}

fn switches_off(path: &syn::Path) -> bool {
    path.is_ident("allow") || path.is_ident("expect")
}

/// The lints an `allow(...)` or `expect(...)` names, with `reason = "…"` left out.
fn lint_names(list: &syn::MetaList) -> Vec<String> {
    listed(list).map(|(lints, _)| lints).unwrap_or_default()
}

/// The sorted, deduplicated lints a list names and whether it states a reason; `None` if the
/// arguments do not parse.
fn listed(list: &syn::MetaList) -> Option<(Vec<String>, bool)> {
    let args = list
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()?;
    let mut lints: Vec<String> = args
        .iter()
        .filter(|arg| !is_reason(arg))
        .map(named)
        .collect();
    lints.sort();
    lints.dedup();
    Some((lints, args.iter().any(is_reason)))
}

/// Whether this argument is `reason = "…"` with a string literal value.
fn is_reason(meta: &Meta) -> bool {
    let Meta::NameValue(pair) = meta else {
        return false;
    };
    pair.path.is_ident("reason")
        && matches!(
            &pair.value,
            Expr::Lit(ExprLit {
                lit: Lit::Str(_),
                ..
            })
        )
}

/// Whether a file sits under a `tests`, `benches` or `examples` directory, judged by path alone.
fn test_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
        .iter()
        .any(|part| matches!(*part, "tests" | "benches" | "examples"))
}

/// Whether this callee resolves to the item these segments name. A path with generics, a qualified
/// self type, or a `crate`, `self` or `super` head never matches.
fn calls(callee: &Expr, imports: &Imports, item: &[&str]) -> bool {
    let Expr::Path(path) = callee else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    let Some(written) = plain(&path.path) else {
        return false;
    };
    let resolved = match path.path.leading_colon {
        Some(_) => Some(written),
        None => imports.resolve(&written),
    };
    resolved.is_some_and(|found| found.iter().map(String::as_str).eq(item.iter().copied()))
}

fn plain(path: &syn::Path) -> Option<Vec<String>> {
    path.segments
        .iter()
        .map(|segment| {
            matches!(segment.arguments, syn::PathArguments::None).then(|| segment.ident.to_string())
        })
        .collect()
}

fn is_inner(attr: &Attribute) -> bool {
    matches!(attr.style, AttrStyle::Inner(_))
}

fn line_of(at: LineColumn) -> u32 {
    u32::try_from(at.line).unwrap_or(u32::MAX)
}

/// A span's byte offset. The column counts characters, not bytes, so its line is walked.
fn offset(src: &str, starts: &[usize], at: LineColumn) -> usize {
    let Some(&from) = starts.get(at.line.saturating_sub(1)) else {
        return src.len();
    };
    let to = starts.get(at.line).copied().unwrap_or(src.len());
    let line = src.get(from..to).unwrap_or_default();
    from + line
        .char_indices()
        .nth(at.column)
        .map_or(line.len(), |(index, _)| index)
}

/// Whether `<tool>: ignore[<rule>] <why>` on the line or the one above waives the rule. Any tool's
/// name counts, so a site another checker already waives needs no second marker.
fn waived(src: &str, line: u32, rule: &str) -> bool {
    let at = usize::try_from(line).unwrap_or(usize::MAX);
    let lines: Vec<&str> = src.lines().collect();
    [at.checked_sub(1), at.checked_sub(2)]
        .into_iter()
        .flatten()
        .filter_map(|i| lines.get(i))
        .any(|text| also_known_as(rule).iter().any(|name| reasoned(text, name)))
}

/// The waiver names a rule accepts: its own, and another checker's name for exactly the same shape.
fn also_known_as(rule: &str) -> Vec<String> {
    let mut names = vec![format!("ignore[{rule}]")];
    if rule == DYNAMIC_SHELL {
        // Outpost's name for this shape.
        names.push("ignore[interpolated-command]".to_string());
    }
    names
}

/// Whether this path is a crate root: `lib.rs`, `main.rs` or a file directly in `src/bin`.
pub(super) fn is_crate_root(path: &str) -> bool {
    let Some(tail) = path.rsplit_once("src/").map(|(_, rest)| rest) else {
        return false;
    };
    matches!(tail, "lib.rs" | "main.rs")
        || (tail.starts_with("bin/") && tail.matches('/').count() == 1)
}
