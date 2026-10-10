//! Lints suppressed without a reason, and two security shapes. The source is parsed, so an `allow`
//! inside a string or a comment does not count.

mod interpolation;
pub mod shipped;
pub(crate) mod targets;
mod testonly;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::LineColumn;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{AttrStyle, Attribute, Expr, ExprLit, Lit, Meta, Token, UseTree};

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind};
use interpolation::{dynamic, is_true, only, text};
use targets::TARGET_DIRS;
use testonly::{test_gate, test_modules};

pub const GATE: Gate = Gate {
    name: "source",
    about: "lints blanket-disabled or suppressed without a reason, and two security shapes",
    group: Group::Quality,
    builds: false,
    reads: None,
    // A ratchet: a mature tree can hold many suppressions, so the gate only stops them growing.
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "suppression(s)",
    },
};

const CRATE_LEVEL: &str = "crate_level_allow";
const STACKED: &str = "stacked_allow_attribute";
const UNREASONED: &str = "unreasoned_allow_attribute";
const SHIPPED_SAFETY: &str = "safety_lint_allowed_in_shipped_code";
const DISABLED_TLS: &str = "disabled_tls_verification";
const DYNAMIC_SHELL: &str = "dynamic_shell_command";

/// The paths to reqwest's client builder; a method chain counts only when it starts from one.
const TLS_BUILDERS: [&[&str]; 2] = [
    &["reqwest", "Client", "builder"],
    &["reqwest", "blocking", "Client", "builder"],
];

const COMMAND_NEW: &[&str] = &["std", "process", "Command", "new"];

/// The programs that read their `-c` argument as a script; `git -c`, for one, sets a config key.
const SHELLS: [&str; 4] = ["bash", "dash", "sh", "zsh"];

fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut series = Series::new();
    for finding in found_in(ctx)? {
        // Keyed by file and rule, not line, so an edit above a suppression is not new debt.
        let key = match &finding.item {
            Some(rule) => format!("{}#{rule}", finding.file),
            None => finding.file.clone(),
        };
        series.set(&key, series.get(&key).unwrap_or(0) + 1);
    }
    Ok(series)
}

fn found_in(ctx: &Ctx) -> Result<Vec<Finding>, String> {
    let (sources, crates) = sources(ctx)?;
    let judge = Judge {
        reached: shipped::reached(&project::metadata(&ctx.root)?, &ctx.not_shipped)?,
        crates: &crates,
        not_shipped: &ctx.not_shipped,
    };
    let mut texts = texts_of(&ctx.root, &sources)?;
    texts.extend(declared_beyond(&ctx.root, &crates, &texts)?);
    let mut findings = Vec::new();
    // A test-only `mod x;` makes `x.rs` test code, which `x.rs` alone does not show, so the files
    // such declarations name are judged again below.
    let mut gated: BTreeSet<String> = BTreeSet::new();
    let mut read: Vec<&(String, String)> = Vec::new();
    for text in &texts {
        let (shown, src) = text;
        match judge.of(shown, src, false) {
            Ok(found) => {
                gated.extend(judge.test_modules(shown, src));
                read.push(text);
                findings.extend(found);
            }
            // A file outside crate code, or one only `include!` reads, may not parse on purpose.
            Err(_)
                if !project::is_crate_code(shown, &crates)
                    || project::only_included(&ctx.root, shown) => {}
            Err(why) => return Err(format!("{shown}: {why}")),
        }
    }
    for (shown, src) in read.into_iter().filter(|(shown, _)| gated.contains(shown)) {
        findings.retain(|found| &found.file != shown);
        findings.extend(judge.of(shown, src, true)?);
    }
    findings.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(findings)
}

/// Each source's text, keyed by root-relative path. A non-UTF-8 file is skipped; rustc rejects it.
fn texts_of(root: &Path, sources: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    let mut texts = Vec::new();
    for path in sources {
        let shown = project::relative(root, path);
        if let Some(src) = readable(path, &shown)? {
            texts.push((shown, src));
        }
    }
    Ok(texts)
}

fn readable(path: &Path, shown: &str) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(src) => Ok(Some(src)),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Ok(None),
        Err(e) => Err(format!("{shown}: {e}")),
    }
}

/// Modules compiled files declare outside what `is_target` covers, such as a `mod.rs` under
/// `tests/` that integration tests share.
fn declared_beyond(
    root: &Path,
    crates: &[String],
    texts: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let mut known: BTreeSet<String> = texts.iter().map(|(shown, _)| shown.clone()).collect();
    let mut pending: Vec<String> = texts
        .iter()
        .flat_map(|(shown, src)| declared(src, shown, crates))
        .collect();
    let mut found = Vec::new();
    while let Some(module) = pending.pop() {
        let Some(path) = project::in_tree(root, &module) else {
            continue;
        };
        if !path.is_file() || !known.insert(module.clone()) {
            continue;
        }
        if let Some(src) = readable(&path, &module)? {
            pending.extend(declared(&src, &module, crates));
            found.push((module, src));
        }
    }
    Ok(found)
}

/// The files every `mod x;` in the file `shown` may name, relative to the root.
fn declared(src: &str, shown: &str, crates: &[String]) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let (dir, base) = (testonly::directory(shown), modules_beside(shown, crates));
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(held) if held.semi.is_some() => Some(held),
            _ => None,
        })
        .flat_map(|held| testonly::named_files(held, dir, &base))
        .map(|candidate| candidate.trim_start_matches('/').to_string())
        .collect()
}

/// The directory a `mod x;` in this file looks in. An integration test, example, bench or `src/bin`
/// file is a crate root, so its modules sit beside it.
fn modules_beside(shown: &str, crates: &[String]) -> String {
    let rooted = crates
        .iter()
        .filter_map(|dir| project::under(shown, dir))
        .any(|rest| {
            let parts: Vec<&str> = rest.split('/').collect();
            matches!(
                parts.as_slice(),
                ["build.rs"] | ["tests" | "examples" | "benches", _] | ["src", "bin", _]
            )
        });
    match (rooted, shown.rsplit_once('/')) {
        (true, Some((dir, _))) => dir.to_string(),
        (true, None) => String::new(),
        (false, _) => testonly::beside(shown),
    }
}

/// What every file is judged against, gathered once and shared by both passes.
struct Judge<'a> {
    reached: shipped::Reached,
    crates: &'a [String],
    not_shipped: &'a [String],
}

impl Judge<'_> {
    /// Whether a feature of this file's crate is test-only; in a crate the manifests miss, none is.
    fn test_only(&self, path: &str) -> impl Fn(&str) -> bool + use<'_> {
        let held = self.reached.get(&owning_crate(path, self.crates));
        move |name: &str| held.is_some_and(|held| !held.contains(name))
    }

    fn of(&self, path: &str, src: &str, gated: bool) -> Result<Vec<Finding>, String> {
        let ships = project::ships(path, self.not_shipped);
        judged_with(path, src, ships, &self.test_only(path), gated)
    }

    fn test_modules(&self, path: &str, src: &str) -> Vec<String> {
        test_modules(src, path, &self.test_only(path))
    }
}

/// The deepest crate directory holding a file, `""` for the project root.
fn owning_crate(path: &str, crates: &[String]) -> String {
    crates
        .iter()
        .filter(|dir| project::under(path, dir).is_some())
        .max_by_key(|dir| dir.len())
        .cloned()
        .unwrap_or_default()
}

/// The findings in one file's source. `ships` is false for code nobody receives, such as a fuzz
/// target, which reports by panicking; the shipped-code rule then does not apply.
pub fn judged(path: &str, src: &str, ships: bool) -> Result<Vec<Finding>, String> {
    judged_with(path, src, ships, &|_| false, false)
}

/// `judged`, given which features are test-only and whether a parent's `mod x;` already made this
/// file test code (`gated`).
pub fn judged_with(
    path: &str,
    src: &str,
    ships: bool,
    test_only: &dyn Fn(&str) -> bool,
    gated: bool,
) -> Result<Vec<Finding>, String> {
    let file = syn::parse_file(src).map_err(|e| format!("line {}: {e}", e.span().start().line))?;
    let mut scan = Scan::new(path, src, Imports::of(&file), ships, test_only);
    scan.test_file |= gated || test_gate(&file.attrs, test_only);
    scan.visit_file(&file);
    Ok(scan.into_findings())
}

/// One `allow(...)` attribute as the suppression rules read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    /// Written the way the source writes them, `clippy::` prefix and all, sorted and deduplicated.
    lints: Vec<String>,
    reasoned: bool,
}

/// One attribute of any kind and its span; all are kept so an attribute between two allows does not
/// split a stack.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    inner: bool,
    from: usize,
    to: usize,
    line: u32,
    allow: Option<Reading>,
    /// Every lint this attribute switches off, through `allow`, `expect` or `cfg_attr`.
    suppresses: Vec<String>,
    in_tests: bool,
}

struct Scan<'a> {
    path: &'a str,
    src: &'a str,
    starts: Vec<usize>,
    imports: Imports,
    attrs: Vec<Seen>,
    /// How many `#[cfg(test)]` items or `#[test]` functions enclose the node being visited.
    tests: usize,
    test_file: bool,
    /// Whether anybody receives this file; see `judged`.
    ships: bool,
    test_only: TestOnly<'a>,
    findings: Vec<Finding>,
}

/// Whether a named feature is one no shipped build enables, read from the workspace manifests.
type TestOnly<'a> = &'a dyn Fn(&str) -> bool;

impl<'a> Scan<'a> {
    fn new(path: &'a str, src: &'a str, imports: Imports, ships: bool, only: TestOnly<'a>) -> Self {
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
            ships,
            test_only: only,
            findings: Vec::new(),
        }
    }

    fn into_findings(mut self) -> Vec<Finding> {
        self.attrs.sort_by_key(|seen| seen.from);
        let mut found = self.findings;
        found.extend(unreasoned(self.path, &self.attrs));
        if self.ships {
            found.extend(shipped(self.path, &self.attrs));
        }
        found.extend(stacked(self.path, self.src, &self.attrs));
        found.sort_by(|a, b| (a.line, &a.item).cmp(&(b.line, &b.item)));
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

    fn in_tests(&self) -> bool {
        self.tests > 0 || self.test_file
    }

    fn report(&mut self, at: LineColumn, rule: &str, message: &str) {
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

/// Every `#[allow]` that switches a lint off without a `reason`.
fn unreasoned(path: &str, attrs: &[Seen]) -> Vec<Finding> {
    attrs
        .iter()
        .filter_map(|seen| {
            let reading = seen.allow.as_ref().filter(|reading| !reading.reasoned)?;
            let bang = if seen.inner { "!" } else { "" };
            let lints = reading.lints.join(", ");
            let message =
                format!("#{bang}[allow({lints})] switches a lint off with no stated reason");
            Some(Finding::at(path, &message).line(seen.line).item(UNREASONED))
        })
        .collect()
}

/// The lints that keep panics and `unsafe` out of shipped code. Allowing one outside test code is a
/// finding even with a reason.
pub(crate) const NEVER_SHIPPED: [&str; 4] = [
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
    "unsafe_code",
];

fn shipped(path: &str, attrs: &[Seen]) -> Vec<Finding> {
    attrs
        .iter()
        .filter(|seen| !seen.in_tests)
        .filter_map(|seen| {
            let named: Vec<&str> = seen
                .suppresses
                .iter()
                .map(String::as_str)
                .filter(|lint| NEVER_SHIPPED.contains(lint))
                .collect();
            if named.is_empty() {
                return None;
            }
            let bang = if seen.inner { "!" } else { "" };
            let message = format!(
                "#{bang}[allow({})] lets a panic or an unsafe block into shipped code, which a \
                 stated reason does not change",
                named.join(", ")
            );
            Some(
                Finding::at(path, &message)
                    .line(seen.line)
                    .item(SHIPPED_SAFETY),
            )
        })
        .collect()
}

/// Items carrying two or more `#[allow]` attributes, or one naming four or more lints.
fn stacked(path: &str, src: &str, attrs: &[Seen]) -> Vec<Finding> {
    runs(src, attrs)
        .into_iter()
        .filter_map(|run| stack(path, run))
        .collect()
}

fn stack(path: &str, run: &[Seen]) -> Option<Finding> {
    let allows: Vec<(&Seen, &Reading)> = run
        .iter()
        .filter(|seen| !seen.inner)
        .filter_map(|seen| Some((seen, seen.allow.as_ref()?)))
        .collect();
    let (first, _) = allows.first()?;
    let widest = allows.iter().map(|(_, r)| r.lints.len()).max().unwrap_or(0);
    if allows.len() < 2 && widest < 4 {
        return None;
    }
    let mut lints: Vec<String> = allows.iter().flat_map(|(_, r)| r.lints.clone()).collect();
    lints.sort();
    lints.dedup();
    let message = if allows.len() >= 2 {
        format!(
            "{} allow attributes stack {} suppressions on one item",
            allows.len(),
            lints.len()
        )
    } else {
        format!(
            "#[allow({})] stacks {} suppressions on one item",
            lints.join(", "),
            lints.len()
        )
    };
    Some(Finding::at(path, &message).line(first.line).item(STACKED))
}

/// Groups of attributes separated only by whitespace and comments: those on the same item.
fn runs<'a>(src: &str, attrs: &'a [Seen]) -> Vec<&'a [Seen]> {
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

fn named(meta: &Meta) -> String {
    meta.path()
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Whether a file sits under a `tests`, `benches` or `examples` directory, judged by path alone.
fn test_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
        .iter()
        .any(|part| matches!(*part, "tests" | "benches" | "examples"))
}

/// The file's `use` bindings, so an imported name and its full path resolve to the same item.
#[derive(Debug, Default, PartialEq, Eq)]
struct Imports {
    bound: BTreeMap<String, Vec<String>>,
    glob: bool,
}

impl Imports {
    fn of(file: &syn::File) -> Self {
        let mut found = Self::default();
        found.visit_file(file);
        found
    }

    /// The full path a written path names. An unbound head is taken as a crate name, unless a glob
    /// import could have bound it, which gives `None`.
    fn resolve(&self, written: &[String]) -> Option<Vec<String>> {
        let (first, rest) = written.split_first()?;
        if matches!(first.as_str(), "crate" | "self" | "super" | "Self") {
            return None;
        }
        match self.bound.get(first) {
            Some(path) => Some(path.iter().chain(rest).cloned().collect()),
            None if self.glob => None,
            None => Some(written.to_vec()),
        }
    }

    fn bind(&mut self, tree: &UseTree, prefix: &mut Vec<String>) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.bind(&path.tree, prefix);
                prefix.pop();
            }
            UseTree::Name(name) => self.name(&name.ident, &name.ident, prefix),
            UseTree::Rename(rename) => self.name(&rename.ident, &rename.rename, prefix),
            UseTree::Glob(_) => self.glob = true,
            UseTree::Group(group) => {
                for item in &group.items {
                    self.bind(item, prefix);
                }
            }
        }
    }

    fn name(&mut self, ident: &syn::Ident, under: &syn::Ident, prefix: &[String]) {
        let mut full = prefix.to_vec();
        full.push(ident.to_string());
        self.bound.insert(under.to_string(), full);
    }
}

impl<'ast> Visit<'ast> for Imports {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.bind(&node.tree, &mut Vec::new());
    }
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

/// Every `.rs` file cargo compiles outside fixture crates, in a stable order, and the crate
/// directories found on the same walk.
fn sources(ctx: &Ctx) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let root = &ctx.root;
    let found = project::walked(
        &ctx.listing,
        root,
        &|name| !project::SKIPPED.contains(&name),
        &|name, path| {
            (name == "Cargo.toml" || name.ends_with(".rs"))
                && !project::is_compile_fail_fixture(path)
        },
    )?;
    let shown: Vec<String> = found
        .iter()
        .map(|path| project::relative(root, path))
        .collect();
    let dirs: Vec<String> = shown
        .iter()
        .filter_map(|path| project::crate_dir(path))
        .collect();
    let fixtures = project::fixture_crates(&dirs);
    let kept = found
        .into_iter()
        .zip(shown)
        .filter(|(_, shown)| shown.ends_with(".rs") && compiled(shown, &dirs))
        .filter(|(_, shown)| {
            !fixtures
                .iter()
                .any(|dir| project::under(shown, dir).is_some())
        })
        .map(|(path, _)| path)
        .collect();
    Ok((kept, dirs))
}

/// Whether `<tool>: ignore[<rule>] <why>` on the line or the one above waives the rule. Any tool's
/// name counts, so a site another checker already waives needs no second marker.
fn waived(src: &str, line: u32, rule: &str) -> bool {
    let at = line as usize;
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

/// Whether the text names the waiver followed by a reason of at least `LEAST_REASON` characters.
fn reasoned(text: &str, want: &str) -> bool {
    text.find(want)
        .is_some_and(|at| text[at + want.len()..].trim().chars().count() >= LEAST_REASON)
}

/// Short enough to allow a terse reason, long enough that a stray word is not one.
const LEAST_REASON: usize = 8;

/// Whether this path is a crate root: `lib.rs`, `main.rs` or a file directly in `src/bin`.
fn is_crate_root(path: &str) -> bool {
    let Some(tail) = path.rsplit_once("src/").map(|(_, rest)| rest) else {
        return false;
    };
    matches!(tail, "lib.rs" | "main.rs")
        || (tail.starts_with("bin/") && tail.matches('/').count() == 1)
}

fn compiled(shown: &str, crates: &[String]) -> bool {
    crates
        .iter()
        .filter_map(|dir| project::under(shown, dir))
        .any(is_target)
}

/// Whether this gate skips one file: cargo does not compile it, or it sits in a fixture crate.
/// Decided from the file's ancestors, so the editor hook needs no tree walk.
pub(crate) fn unread(root: &Path, shown: &str) -> bool {
    let crates: Vec<String> = Path::new(shown)
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|dir| root.join(dir).join(project::MANIFEST).is_file())
        .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        .collect();
    let fixtures = project::fixture_crates(&crates);
    (!compiled(shown, &crates) && !targets::declared_by_a_target(root, shown, &crates))
        || fixtures
            .iter()
            .any(|dir| project::under(shown, dir).is_some())
}

/// Whether cargo builds this crate-relative path: anything under `src/`, the build script, a target
/// file, or a target subdirectory's `main.rs`.
fn is_target(rest: &str) -> bool {
    let mut parts = rest.split('/');
    let Some(head) = parts.next() else {
        return false;
    };
    if head == "build.rs" {
        return true;
    }
    if head == "src" {
        return parts.next().is_some();
    }
    if !TARGET_DIRS.contains(&head) {
        return false;
    }
    // Anything deeper compiles only if a target declares it, which `declared_beyond` follows.
    match (parts.next(), parts.next(), parts.next()) {
        (Some(name), None, _) => name.ends_with(".rs"),
        (Some(_), Some("main.rs"), None) => true,
        _ => false,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn faults(path: &str, src: &str) -> Result<Vec<Finding>, String> {
        judged(path, src, true)
    }

    fn rendered(src: &str) -> Vec<String> {
        at("src/lib.rs", src)
    }

    fn at(path: &str, src: &str) -> Vec<String> {
        faults(path, src)
            .unwrap()
            .iter()
            .map(Finding::render)
            .collect()
    }

    fn only_rule(rule: &str, src: &str) -> Vec<String> {
        faults("src/lib.rs", src)
            .unwrap()
            .iter()
            .filter(|found| found.item.as_deref() == Some(rule))
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn a_blanket_allow_over_a_whole_file_is_reported_whatever_its_reason() {
        assert_eq!(
            only_rule(
                CRATE_LEVEL,
                "#![allow(clippy::unwrap_used)]\nfn free() {}\n"
            ),
            [
                "src/lib.rs:1: crate_level_allow: #![allow(clippy::unwrap_used)] covers this whole file"
            ]
        );
        assert_eq!(
            only_rule(
                CRATE_LEVEL,
                "#![allow(dead_code, reason = \"scope is what is judged\")]\n"
            ),
            ["src/lib.rs:1: crate_level_allow: #![allow(dead_code)] covers this whole file"]
        );
    }

    #[test]
    fn a_blanket_allow_over_an_inline_module_names_that_module() {
        assert_eq!(
            only_rule(
                CRATE_LEVEL,
                "mod imports {\n    #![allow(unused_imports)]\n    pub use std::fs;\n}\n"
            ),
            [
                "src/lib.rs:2: crate_level_allow: #![allow(unused_imports)] covers the whole module `imports`"
            ]
        );
    }

    #[test]
    fn an_inner_attribute_that_is_no_allow_does_not_hide_the_blanket_behind_it() {
        assert_eq!(
            only_rule(
                CRATE_LEVEL,
                "#![deny(dead_code)]\n#![allow(clippy::unwrap_used)]\n"
            ),
            [
                "src/lib.rs:2: crate_level_allow: #![allow(clippy::unwrap_used)] covers this whole file"
            ]
        );
    }

    #[test]
    fn an_inner_allow_narrower_than_a_module_exempts_one_item_and_is_not_a_blanket() {
        assert_eq!(
            only_rule(CRATE_LEVEL, "fn free() {\n    #![allow(dead_code)]\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_blanket_allow_over_test_code_asks_for_a_scope_that_does_not_exist() {
        let gated = "#[cfg(test)]\nmod tests {\n    #![allow(clippy::unwrap_used)]\n}\n";
        assert_eq!(only_rule(CRATE_LEVEL, gated), Vec::<String>::new());
        let narrowed = "#[cfg(all(test, unix))]\nmod tests {\n    #![allow(dead_code)]\n}\n";
        assert_eq!(only_rule(CRATE_LEVEL, narrowed), Vec::<String>::new());
        assert_eq!(
            at(
                "tests/cli.rs",
                "#![allow(clippy::unwrap_used, reason = \"tests unwrap\")]\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cfg_that_still_builds_outside_a_test_run_is_not_a_test_gate() {
        for shipped in [
            "#[cfg(any(test, feature = \"x\"))]\nmod m {\n    #![allow(dead_code)]\n}\n",
            "#[cfg(not(test))]\nmod m {\n    #![allow(dead_code)]\n}\n",
            "#[cfg(test = \"yes\")]\nmod m {\n    #![allow(dead_code)]\n}\n",
        ] {
            assert_eq!(only_rule(CRATE_LEVEL, shipped).len(), 1, "{shipped}");
        }
    }

    #[test]
    fn a_deny_a_warn_and_an_empty_allow_are_not_suppressions() {
        for quiet in [
            "#![deny(dead_code)]\n",
            "#![warn(dead_code)]\n",
            "#![doc = \"a crate\"]\n",
            "#![allow()]\n",
        ] {
            assert_eq!(rendered(quiet), Vec::<String>::new(), "{quiet}");
        }
    }

    #[test]
    fn an_allow_attribute_carrying_a_reason_is_not_reported() {
        assert_eq!(
            rendered("#[allow(dead_code, reason = \"the surface is public\")]\nfn free() {}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_allow_attribute_with_no_reason_names_the_lints_it_switched_off() {
        assert_eq!(
            only_rule(UNREASONED, "#[allow(dead_code)]\nfn free() {}\n"),
            [
                "src/lib.rs:1: unreasoned_allow_attribute: #[allow(dead_code)] switches a lint off with no stated reason"
            ]
        );
    }

    #[test]
    fn an_inner_allow_with_no_reason_is_reported_as_the_inner_attribute_it_is() {
        assert_eq!(
            only_rule(UNREASONED, "#![allow(dead_code)]\n"),
            [
                "src/lib.rs:1: unreasoned_allow_attribute: #![allow(dead_code)] switches a lint off with no stated reason"
            ]
        );
    }

    #[test]
    fn an_expect_attribute_and_a_gated_allow_are_both_left_alone() {
        for quiet in [
            "#[expect(dead_code)]\nfn free() {}\n",
            "#[cfg_attr(test, allow(dead_code))]\nfn free() {}\n",
            "#[deny(dead_code)]\nfn free() {}\n",
        ] {
            assert_eq!(rendered(quiet), Vec::<String>::new(), "{quiet}");
        }
    }

    #[test]
    fn a_reasoned_two_lint_exemption_on_a_test_module_is_reported_by_no_rule() {
        let src = "#[cfg(test)]\n#[allow(\n    clippy::unwrap_used,\n    clippy::panic,\n    \
                   reason = \"a failed unwrap in a test is the test failing\"\n)]\nmod tests {}\n";
        assert_eq!(rendered(src), Vec::<String>::new());
    }

    #[test]
    fn two_allow_attributes_on_one_item_are_a_stack() {
        assert_eq!(
            only_rule(
                STACKED,
                "#[allow(dead_code)]\n#[allow(unused_variables)]\nfn f() {}\n"
            ),
            [
                "src/lib.rs:1: stacked_allow_attribute: 2 allow attributes stack 2 suppressions on one item"
            ]
        );
    }

    #[test]
    fn one_attribute_naming_four_lints_is_the_same_act_as_four_attributes() {
        assert_eq!(
            only_rule(STACKED, "#[allow(a, b, c, d)]\nfn wide() {}\n"),
            [
                "src/lib.rs:1: stacked_allow_attribute: #[allow(a, b, c, d)] stacks 4 suppressions on one item"
            ]
        );
    }

    #[test]
    fn three_lints_under_one_attribute_stay_under_the_bar() {
        assert_eq!(
            only_rule(STACKED, "#[allow(a, b, c)]\nfn three() {}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_reason_is_not_a_lint_so_it_never_pushes_an_attribute_over_the_bar() {
        assert_eq!(
            only_rule(
                STACKED,
                "#[allow(a, b, c, reason = \"still three lints\")]\nfn f() {}\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn allow_attributes_on_two_different_items_are_not_one_stack() {
        assert_eq!(
            only_rule(
                STACKED,
                "#[allow(a)]\nfn one() {}\n#[allow(b)]\nfn other() {}\n"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            only_rule(
                STACKED,
                "#[allow(a)] fn one() {} #[allow(b)] fn other() {}\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_unrelated_attribute_between_two_allows_does_not_break_the_stack() {
        assert_eq!(
            only_rule(
                STACKED,
                "#[allow(a)]\n#[cfg(unix)]\n#[allow(b)]\nfn f() {}\n"
            ),
            [
                "src/lib.rs:1: stacked_allow_attribute: 2 allow attributes stack 2 suppressions on one item"
            ]
        );
    }

    #[test]
    fn a_comment_between_two_allows_does_not_break_the_stack() {
        for joined in [
            "#[allow(a)]\n// why\n#[allow(b)]\nfn f() {}\n",
            "#[allow(a)]\n/* why */\n#[allow(b)]\nfn f() {}\n",
            "#[allow(a)]\n/* /* nested */ */\n#[allow(b)]\nfn f() {}\n",
            "#[allow(a)]\n/// why\n#[allow(b)]\nfn f() {}\n",
        ] {
            assert_eq!(only_rule(STACKED, joined).len(), 1, "{joined}");
        }
    }

    #[test]
    fn the_same_lint_written_twice_stacks_two_attributes_over_one_suppression() {
        assert_eq!(
            only_rule(STACKED, "#[allow(a)]\n#[allow(a)]\nfn f() {}\n"),
            [
                "src/lib.rs:1: stacked_allow_attribute: 2 allow attributes stack 1 suppressions on one item"
            ]
        );
    }

    #[test]
    fn an_inner_allow_is_no_part_of_the_stack_on_the_item_below_it() {
        assert_eq!(
            only_rule(
                STACKED,
                "mod m {\n    #![allow(a)]\n    #[allow(b)]\n    fn f() {}\n}\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_stack_on_a_struct_field_is_found_the_same_way_as_one_on_a_function() {
        assert_eq!(
            only_rule(
                STACKED,
                "struct S {\n    #[allow(a)]\n    #[allow(b)]\n    field: u8,\n}\n"
            )
            .len(),
            1
        );
    }

    #[test]
    fn an_allow_written_inside_a_string_literal_is_not_an_attribute() {
        let src = "const A: &str = \"#[allow(dead_code)]\";\nconst B: &str = \"#![allow(a)]\";\n// #[allow(x)]\n";
        assert_eq!(rendered(src), Vec::<String>::new());
    }

    #[test]
    fn a_file_that_will_not_parse_is_a_failure_to_run_not_a_pass() {
        assert!(
            faults("src/lib.rs", "fn f( {\n")
                .unwrap_err()
                .starts_with("line 1")
        );
    }

    #[test]
    fn a_reqwest_builder_told_to_accept_an_invalid_certificate_is_reported() {
        assert_eq!(
            rendered(
                "fn c() {\n    let _ = reqwest::Client::builder().danger_accept_invalid_certs(true);\n}\n"
            ),
            [
                "src/lib.rs:2: disabled_tls_verification: reqwest client builder disables TLS certificate verification"
            ]
        );
    }

    #[test]
    fn the_blocking_builder_and_the_hostname_switch_are_recognised_too() {
        let src = "fn c() {\n    let _ = reqwest::blocking::Client::builder()\n        \
                   .tls_danger_accept_invalid_hostnames(true);\n}\n";
        assert_eq!(
            rendered(src),
            [
                "src/lib.rs:3: disabled_tls_verification: reqwest client builder disables TLS hostname verification"
            ]
        );
    }

    #[test]
    fn an_imported_or_renamed_client_resolves_to_the_same_builder() {
        for imported in [
            "use reqwest::Client;\nfn c() {\n    let _ = Client::builder().danger_accept_invalid_certs(true);\n}\n",
            "use reqwest::Client as Http;\nfn c() {\n    let _ = Http::builder().danger_accept_invalid_certs(true);\n}\n",
            "use reqwest as http;\nfn c() {\n    let _ = http::Client::builder().danger_accept_invalid_certs(true);\n}\n",
        ] {
            assert_eq!(only_rule(DISABLED_TLS, imported).len(), 1, "{imported}");
        }
    }

    #[test]
    fn a_builder_call_reached_through_a_longer_chain_is_still_the_same_builder() {
        let src = "fn c() {\n    let _ = reqwest::Client::builder()\n        .timeout(t)\n        \
                   .danger_accept_invalid_certs(true);\n}\n";
        assert_eq!(only_rule(DISABLED_TLS, src).len(), 1);
    }

    #[test]
    fn the_same_method_name_on_something_that_is_not_a_reqwest_builder_is_left_alone() {
        for quiet in [
            "fn c() {\n    let _ = mine::Client::builder().danger_accept_invalid_certs(true);\n}\n",
            "fn c() {\n    let _ = self.client.danger_accept_invalid_certs(true);\n}\n",
            "fn c() {\n    let _ = reqwest::Client::new().danger_accept_invalid_certs(true);\n}\n",
            "fn c() {\n    let _ = reqwest::Client::builder(cfg).danger_accept_invalid_certs(true);\n}\n",
        ] {
            assert_eq!(
                only_rule(DISABLED_TLS, quiet),
                Vec::<String>::new(),
                "{quiet}"
            );
        }
    }

    #[test]
    fn verification_left_on_is_not_a_finding() {
        assert_eq!(
            only_rule(
                DISABLED_TLS,
                "fn c() {\n    let _ = reqwest::Client::builder().danger_accept_invalid_certs(false);\n}\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_disabled_verification_in_test_code_is_left_alone() {
        let gated = "#[cfg(test)]\nmod tests {\n    fn c() {\n        let _ = \
                     reqwest::Client::builder().danger_accept_invalid_certs(true);\n    }\n}\n";
        assert_eq!(only_rule(DISABLED_TLS, gated), Vec::<String>::new());
        let marked = "#[test]\nfn c() {\n    let _ = reqwest::Client::builder().danger_accept_invalid_certs(true);\n}\n";
        assert_eq!(only_rule(DISABLED_TLS, marked), Vec::<String>::new());
        let under = "fn c() {\n    let _ = reqwest::Client::builder().danger_accept_invalid_certs(true);\n}\n";
        assert_eq!(at("tests/wire.rs", under), Vec::<String>::new());
    }

    #[test]
    fn a_method_is_test_code_only_when_it_is_marked_itself() {
        let src = "impl S {\n    #[test]\n    fn t() {\n        let _ = \
                   reqwest::Client::builder().danger_accept_invalid_certs(true);\n    }\n    \
                   fn c() {\n        let _ = \
                   reqwest::Client::builder().danger_accept_invalid_certs(true);\n    }\n}\n";
        assert_eq!(
            rendered(src),
            [
                "src/lib.rs:7: disabled_tls_verification: reqwest client builder disables TLS certificate verification"
            ]
        );
    }

    #[test]
    fn a_glob_import_makes_the_provenance_undecidable_and_the_rule_abstains() {
        let src = "use whatever::*;\nfn c() {\n    let _ = Client::builder().danger_accept_invalid_certs(true);\n}\n";
        assert_eq!(only_rule(DISABLED_TLS, src), Vec::<String>::new());
    }

    #[test]
    fn a_shell_command_interpolated_by_format_is_reported() {
        let src = "use std::process::Command;\nfn run(name: &str) {\n    let _ = Command::new(\"sh\")\n        \
                   .arg(\"-c\")\n        .arg(format!(\"ls {name}\"));\n}\n";
        assert_eq!(
            rendered(src),
            [
                "src/lib.rs:5: dynamic_shell_command: a dynamic value is interpolated into a shell command"
            ]
        );
    }

    #[test]
    fn a_shell_command_built_by_concatenation_is_reported() {
        let src = "fn run(name: &str) {\n    let _ = std::process::Command::new(\"bash\")\n        \
                   .arg(\"-c\")\n        .arg(\"ls \".to_string() + name);\n}\n";
        assert_eq!(
            only_rule(DYNAMIC_SHELL, src),
            [
                "src/lib.rs:4: dynamic_shell_command: a dynamic value is interpolated into a shell \
              command"
            ]
        );
    }

    #[test]
    fn a_shell_command_written_out_in_full_is_not_a_finding() {
        for quiet in [
            "fn run() {\n    let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(\"ls -l\");\n}\n",
            "fn run() {\n    let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(format!(\"ls {}\", \"here\"));\n}\n",
            "fn run() {\n    let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(format!(\"ls -l\"));\n}\n",
        ] {
            assert_eq!(
                only_rule(DYNAMIC_SHELL, quiet),
                Vec::<String>::new(),
                "{quiet}"
            );
        }
    }

    #[test]
    fn a_waiver_another_checker_wrote_for_the_same_shape_is_honoured() {
        let shell =
            "    let _ = std::process::Command::new(\"sh\").arg(\"-c\").arg(format!(\"ls {d}\"));";
        for marker in [
            format!("    // chock: ignore[{DYNAMIC_SHELL}] git runs a declared hook this way"),
            "    // outpost: ignore[interpolated-command] git runs a declared hook this way"
                .to_string(),
        ] {
            let src = format!("fn run(d: &str) {{\n{marker}\n{shell}\n}}\n");
            assert_eq!(
                only_rule(DYNAMIC_SHELL, &src),
                Vec::<String>::new(),
                "{marker}"
            );
        }
        let reported = [
            "src/lib.rs:3: dynamic_shell_command: a dynamic value is interpolated \
                        into a shell command"
                .to_string(),
        ];
        // Another checker's name for a different shape waives nothing here.
        let wrong = format!(
            "fn run(d: &str) {{\n    // outpost: ignore[unrelated-rule] we accept this one\n{shell}\n}}\n"
        );
        assert_eq!(only_rule(DYNAMIC_SHELL, &wrong), reported);
        // A marker with no reason waives nothing.
        let silent = format!(
            "fn run(d: &str) {{\n    // outpost: ignore[interpolated-command]\n{shell}\n}}\n"
        );
        assert_eq!(only_rule(DYNAMIC_SHELL, &silent), reported);
    }

    #[test]
    fn a_program_that_reads_dash_c_as_something_other_than_a_script_is_left_alone() {
        let src = "fn run(key: &str) {\n    let _ = std::process::Command::new(\"git\")\n        \
                   .arg(\"-c\")\n        .arg(format!(\"user.name={key}\"));\n}\n";
        assert_eq!(only_rule(DYNAMIC_SHELL, src), Vec::<String>::new());
    }

    #[test]
    fn a_shell_handed_a_flag_that_is_not_dash_c_is_left_alone() {
        let src = "fn run(f: &str) {\n    let _ = std::process::Command::new(\"sh\")\n        \
                   .arg(\"-x\")\n        .arg(format!(\"{f}\"));\n}\n";
        assert_eq!(only_rule(DYNAMIC_SHELL, src), Vec::<String>::new());
    }

    #[test]
    fn a_shell_command_whose_format_field_is_dynamic_is_reported() {
        for built in ["\"ls {user}\"", "\"ls {}\", name", "\"ls {n}\", n = name"] {
            let src = format!(
                "fn run() {{ let _ = std::process::Command::new(\"zsh\").arg(\"-c\").arg(format!({built})); }}\n"
            );
            assert_eq!(
                only_rule(DYNAMIC_SHELL, &src),
                [
                    "src/lib.rs:1: dynamic_shell_command: a dynamic value is interpolated into a \
                  shell command"
                ],
                "{built}"
            );
        }
        for written in ["\"ls {}\", \"here\"", "\"ls {{user}}\""] {
            let src = format!(
                "fn run() {{ let _ = std::process::Command::new(\"zsh\").arg(\"-c\").arg(format!({written})); }}\n"
            );
            assert_eq!(
                only_rule(DYNAMIC_SHELL, &src),
                Vec::<String>::new(),
                "{written}"
            );
        }
    }

    #[test]
    fn a_shell_interpolation_inside_a_test_is_still_reported() {
        let src = "#[cfg(test)]\nmod tests {\n    fn run(n: &str) {\n        let _ = \
                   std::process::Command::new(\"sh\").arg(\"-c\").arg(format!(\"ls {n}\"));\n    }\n}\n";
        assert_eq!(only_rule(DYNAMIC_SHELL, src).len(), 1);
    }

    #[test]
    fn a_format_in_a_string_literal_builds_no_command() {
        let src = "const S: &str = \"Command::new(\\\"sh\\\").arg(\\\"-c\\\").arg(format!(\\\"{x}\\\"))\";\n";
        assert_eq!(rendered(src), Vec::<String>::new());
    }

    #[test]
    fn every_finding_is_reported_once_and_in_the_order_the_file_reads() {
        let src = "#![allow(a)]\n#[allow(b)]\n#[allow(c)]\nfn f() {}\n";
        assert_eq!(
            rendered(src),
            [
                "src/lib.rs:1: crate_level_allow: #![allow(a)] covers this whole file",
                "src/lib.rs:1: unreasoned_allow_attribute: #![allow(a)] switches a lint off with no stated reason",
                "src/lib.rs:2: stacked_allow_attribute: 2 allow attributes stack 2 suppressions on one item",
                "src/lib.rs:2: unreasoned_allow_attribute: #[allow(b)] switches a lint off with no stated reason",
                "src/lib.rs:3: unreasoned_allow_attribute: #[allow(c)] switches a lint off with no stated reason",
            ]
        );
    }

    #[test]
    fn a_multi_byte_character_above_an_attribute_moves_no_offset_below_it() {
        let src = "const S: &str = \"—— ——\";\n#[allow(a)]\n#[allow(b)]\nfn f() {}\n";
        assert_eq!(only_rule(STACKED, src).len(), 1);
    }

    #[test]
    fn only_the_files_cargo_compiles_are_read() {
        let here = [String::new(), "crates/a".to_string()];
        assert!(compiled("src/lib.rs", &here));
        assert!(compiled("crates/a/src/gates/mod.rs", &here));
        assert!(compiled("tests/end_to_end.rs", &here));
        assert!(compiled("build.rs", &here));
        assert!(!compiled("docs/sample.rs", &here));
        assert!(!compiled("lib.rs", &here));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_shared_test_module_a_target_declares_is_read_and_an_undeclared_one_is_not() {
        let dir = crate::testdir::make("source-shared-test-module");
        for (path, text) in [
            (
                "Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.0.0\"\n",
            ),
            ("tests/uses.rs", "mod common;\n"),
            ("tests/common/mod.rs", "mod repo;\n"),
            ("tests/common/repo.rs", ""),
            ("tests/orphan/mod.rs", ""),
            ("examples/tour/main.rs", "mod steps;\n"),
            ("examples/tour/steps.rs", ""),
            (
                "crates/inner/Cargo.toml",
                "[package]\nname = \"inner\"\nversion = \"0.0.0\"\n",
            ),
            ("crates/inner/tests/it.rs", "mod shared;\n"),
            ("crates/inner/tests/shared/mod.rs", ""),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        assert!(!unread(&dir, "tests/common/mod.rs"));
        assert!(
            !unread(&dir, "tests/common/repo.rs"),
            "declared by a declared module"
        );
        assert!(
            !unread(&dir, "examples/tour/steps.rs"),
            "declared by a directory target"
        );
        assert!(unread(&dir, "tests/orphan/mod.rs"), "no target declares it");
        assert!(
            unread(&dir, "benches/none/mod.rs"),
            "a directory with no targets"
        );
        assert!(
            !unread(&dir, "crates/inner/tests/shared/mod.rs"),
            "a crate below the root"
        );
    }

    /// Declarations pop in reverse order, so finding `inside` shows the walk skips the escape and
    /// the known file and does not stop.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_module_a_path_attribute_places_outside_the_tree_is_not_read() {
        let outer = crate::testdir::make("source-declared-outside");
        let root = outer.join("project");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        for file in [
            outer.join("escape.rs"),
            root.join("tests/inside.rs"),
            root.join("tests/known.rs"),
        ] {
            std::fs::write(file, "").unwrap();
        }
        let src = "#[path = \"inside.rs\"]\nmod inside;\n#[path = \"known.rs\"]\nmod known;\n\
                   #[path = \"../../escape.rs\"]\nmod escape;\n";
        let texts = [
            ("tests/uses.rs".to_string(), src.to_string()),
            ("tests/known.rs".to_string(), String::new()),
        ];
        let found = declared_beyond(&root, &[String::new()], &texts).unwrap();
        let named: Vec<&str> = found.iter().map(|(module, _)| module.as_str()).collect();
        assert_eq!(named, ["tests/inside.rs"]);
    }

    #[test]
    fn a_file_below_a_targets_own_directory_is_corpus_rather_than_a_target() {
        let here = [String::new()];
        assert!(!compiled("tests/ui/ast_lowering/anon_const.rs", &here));
        assert!(!compiled("tests/ui/case.rs", &here));
        assert!(!compiled("examples/data/sample.rs", &here));
        // The shapes cargo does build.
        assert!(compiled("tests/ui/main.rs", &here));
        assert!(compiled("src/deep/nested/module.rs", &here));
        // A target directory holds files that are not Rust, and nothing below an entry point.
        assert!(!compiled("tests/README", &here));
        assert!(!compiled("tests/ui/main.rs/extra.rs", &here));
    }

    #[test]
    fn a_target_directory_is_read_relative_to_the_crate_that_owns_it() {
        let here = [String::new()];
        assert!(!compiled("vendor/other/src/lib.rs", &here));
        assert!(compiled(
            "vendor/other/src/lib.rs",
            &[String::new(), "vendor/other".to_string()]
        ));
    }

    /// A manifest cargo can read; the empty `[workspace]` stops cargo finding chock's own above it.
    const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\
                            edition = \"2021\"\n[workspace]\n";

    const WITH_MEMBER: &str = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\
                               edition = \"2021\"\n[workspace]\nmembers = [\"benchmark\"]\n";

    const MEMBER: &str = "[package]\nname = \"bench-fixture\"\nversion = \"0.0.0\"\n\
                          edition = \"2021\"\n";

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        crate::testdir::Held::tree("source-gate", files)
    }

    #[test]
    fn a_reason_does_not_let_a_panic_ship() {
        for lint in NEVER_SHIPPED {
            let src = format!("#[allow({lint}, reason = \"we mean it\")]\nfn f() {{}}\n");
            assert_eq!(
                rules("src/gates/a.rs", &src),
                vec![SHIPPED_SAFETY.to_string()],
                "{lint}"
            );
        }
    }

    #[test]
    fn expect_does_not_let_a_panic_ship_either() {
        for lint in NEVER_SHIPPED {
            let src = format!("#[expect({lint}, reason = \"we mean it\")]\nfn f() {{}}\n");
            assert_eq!(
                rules("src/gates/a.rs", &src),
                vec![SHIPPED_SAFETY.to_string()],
                "{lint}"
            );
        }
    }

    #[test]
    fn a_cfg_attr_that_is_not_test_only_still_ships() {
        let not_test =
            "#[cfg_attr(not(test), allow(unsafe_code, reason = \"deliberate\"))]\nfn f() {}\n";
        assert_eq!(
            rules("src/gates/a.rs", not_test),
            vec![SHIPPED_SAFETY.to_string()]
        );
        let feature = "#[cfg_attr(feature = \"x\", expect(clippy::panic))]\nfn f() {}\n";
        assert_eq!(
            rules("src/gates/a.rs", feature),
            vec![SHIPPED_SAFETY.to_string()]
        );
    }

    #[test]
    fn a_cfg_attr_gated_on_test_is_left_alone() {
        let gated = "#![cfg_attr(test, allow(unsafe_code))]\npub fn f() {}\n";
        assert_eq!(rules("crates/a/src/lib.rs", gated), Vec::<String>::new());
    }

    #[test]
    fn a_suppression_written_across_lines_is_still_one() {
        let across = "#[allow(\n    unsafe_code,\n    reason = \"the only production unsafe\"\n)]\nfn f() {}\n";
        assert_eq!(
            rules("src/gates/a.rs", across),
            vec![SHIPPED_SAFETY.to_string()]
        );
    }

    #[test]
    fn test_code_may_still_switch_those_lints_off() {
        let src = "#[cfg(test)]\nmod tests {\n    #![allow(clippy::unwrap_used, reason = \"a failed \
                   unwrap is the test failing\")]\n    fn t() {}\n}\n";
        assert_eq!(rules("src/gates/a.rs", src), Vec::<String>::new());
    }

    #[test]
    fn an_ordinary_lint_is_still_excused_by_a_reason() {
        let src = "#[allow(dead_code, reason = \"the surface is public\")]\nfn f() {}\n";
        assert_eq!(rules("src/gates/a.rs", src), Vec::<String>::new());
    }

    #[test]
    fn an_attribute_naming_both_kinds_reports_only_the_one_that_ships() {
        let src =
            "#[allow(dead_code, clippy::panic, reason = \"a deliberate trade\")]\nfn f() {}\n";
        let found = faults("src/gates/a.rs", src).unwrap();
        let shipped: Vec<String> = found
            .iter()
            .filter(|f| f.item.as_deref() == Some(SHIPPED_SAFETY))
            .map(|f| f.message.clone())
            .collect();
        assert_eq!(
            shipped,
            vec![
                "#[allow(clippy::panic)] lets a panic or an unsafe block into shipped code, which \
                 a stated reason does not change"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_module_file_is_not_where_a_crate_begins() {
        assert!(is_crate_root("src/lib.rs"));
        assert!(is_crate_root("src/main.rs"));
        assert!(is_crate_root("crates/outpost-core/src/lib.rs"));
        assert!(is_crate_root("crates/a/src/bin/tool.rs"));
        assert!(!is_crate_root("crates/outpost-core/src/test.rs"));
        assert!(!is_crate_root("src/gates/tools.rs"));
        assert!(!is_crate_root("src/bin/deep/nested.rs"));
        assert!(!is_crate_root("build.rs"));
    }

    #[test]
    fn a_module_file_keeps_its_reasoned_inner_allow_and_loses_an_unreasoned_one() {
        // Not a never-shipped lint, so only scope is tested.
        let reasoned = "#![allow(dead_code, reason = \"the surface is public\")]\nfn f() {}\n";
        assert_eq!(
            rules("crates/a/src/test.rs", reasoned),
            Vec::<String>::new()
        );
        let silent = "#![allow(dead_code)]\nfn f() {}\n";
        assert_eq!(
            rules("crates/a/src/test.rs", silent),
            vec![UNREASONED.to_string()]
        );
    }

    #[test]
    fn a_crate_root_is_still_flagged_however_well_it_explains_itself() {
        // Not a never-shipped lint, so only scope is tested.
        let reasoned = "#![allow(dead_code, reason = \"the surface is public\")]\nfn f() {}\n";
        assert_eq!(
            rules("crates/a/src/lib.rs", reasoned),
            vec![CRATE_LEVEL.to_string()]
        );
    }

    #[test]
    fn a_feature_no_shipped_build_enables_makes_an_any_test_cfg_a_test_gate() {
        let src = "#[cfg(any(test, feature = \"test-utils\"))]\n\
                   #[allow(clippy::unwrap_used, reason = \"a helper for tests\")]\n\
                   pub fn helper() {}\n";
        // Nothing shipped turns `test-utils` on: the never-shipped rule is not about this.
        let excused = judged_with("src/lib.rs", src, true, &|f| f == "test-utils", false).unwrap();
        assert_eq!(excused, Vec::new());
        // The same feature, reachable from something shipped: it is a suppression that ships.
        let reported: Vec<String> = judged_with("src/lib.rs", src, true, &|_| false, false)
            .unwrap()
            .iter()
            .filter_map(|f| f.item.clone())
            .collect();
        assert_eq!(reported, vec![SHIPPED_SAFETY.to_string()]);
    }

    /// Whether a `clippy::panic` allow under `cfg(<cfg>)` is excused as test code.
    fn any_arm(cfg: &str, test_only: &dyn Fn(&str) -> bool) -> bool {
        let src = format!(
            "#[cfg({cfg})]\n#[allow(clippy::panic, reason = \"deliberate\")]\nfn f() {{}}\n"
        );
        judged_with("src/lib.rs", &src, true, test_only, false)
            .unwrap()
            .is_empty()
    }

    #[test]
    fn every_arm_of_an_any_has_to_be_test_only_and_an_all_needs_only_one() {
        let only_utils = |f: &str| f == "test-utils";
        assert!(any_arm("any(test, feature = \"test-utils\")", &only_utils));
        // `semantic` ships, so this cfg reaches a shipped build.
        assert!(!any_arm(
            "any(test, feature = \"test-utils\", feature = \"semantic\")",
            &only_utils
        ));
        // Nested, and still every arm.
        assert!(any_arm(
            "any(test, any(feature = \"test-utils\", test))",
            &only_utils
        ));
        // `all()` needs only one test-only arm.
        assert!(any_arm("all(test, feature = \"semantic\")", &|_| false));
        // A feature alone, with nothing shipped enabling it.
        assert!(any_arm("feature = \"test-utils\"", &only_utils));
        assert!(!any_arm("feature = \"semantic\"", &only_utils));
    }

    #[test]
    fn a_module_a_parent_gated_test_only_is_test_code_in_the_file_it_names() {
        let parent = "#[cfg(any(test, feature = \"test-utils\"))]\npub mod helpers;\n";
        let named = test_modules(parent, "src/lib.rs", &|f| f == "test-utils");
        assert_eq!(named, vec!["src/helpers.rs", "src/helpers/mod.rs"]);
        // A module the parent did not gate names nothing.
        assert_eq!(
            test_modules("pub mod helpers;\n", "src/lib.rs", &|_| true),
            Vec::<String>::new()
        );
        // A module with a body names no file.
        assert_eq!(
            test_modules(
                "#[cfg(test)]\nmod helpers { fn f() {} }\n",
                "src/lib.rs",
                &|_| true
            ),
            Vec::<String>::new()
        );
        // A source that will not parse names nothing; the file's own judgement reports that.
        assert_eq!(
            test_modules("mod helpers", "src/lib.rs", &|_| true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_module_declaration_is_resolved_the_way_rustc_resolves_one() {
        let gated = "#[cfg(test)]\nmod helpers;\n";
        // A crate root and a `mod.rs` own their own directory.
        assert_eq!(
            test_modules(gated, "crates/a/src/lib.rs", &|_| true),
            vec!["crates/a/src/helpers.rs", "crates/a/src/helpers/mod.rs"]
        );
        assert_eq!(
            test_modules(gated, "src/deep/mod.rs", &|_| true),
            vec!["src/deep/helpers.rs", "src/deep/helpers/mod.rs"]
        );
        // Any other module owns the directory named after it.
        assert_eq!(
            test_modules(gated, "src/deep.rs", &|_| true),
            vec!["src/deep/helpers.rs", "src/deep/helpers/mod.rs"]
        );
        // A `#[path]` replaces both candidates, and `r#` only escapes a keyword.
        assert_eq!(
            test_modules(
                "#[cfg(test)]\n#[path = \"d/y.rs\"]\nmod helpers;\n",
                "src/lib.rs",
                &|_| true
            ),
            vec!["src/d/y.rs"]
        );
        // Beside a file that owns a directory, a `#[path]` still starts from the file's own.
        assert_eq!(
            test_modules(
                "#[cfg(test)]\n#[path = \"step_tests.rs\"]\nmod tests;\n",
                "src/step.rs",
                &|_| true
            ),
            vec!["src/step_tests.rs"]
        );
        assert_eq!(
            test_modules("#[cfg(test)]\nmod r#async;\n", "src/lib.rs", &|_| true),
            vec!["src/async.rs", "src/async/mod.rs"]
        );
    }

    #[test]
    fn no_waiver_comment_excuses_a_never_shipped_lint() {
        for lint in NEVER_SHIPPED {
            for waiver in [
                format!("// chock: ignore[{SHIPPED_SAFETY}] we accept this\n"),
                format!("// chock: ignore[{UNREASONED}] we accept this\n"),
                "// chock: ignore[everything] we accept this\n".to_string(),
            ] {
                let src =
                    format!("{waiver}#[allow({lint}, reason = \"deliberate\")]\nfn f() {{}}\n");
                assert_eq!(
                    rules("src/gates/a.rs", &src),
                    vec![SHIPPED_SAFETY.to_string()],
                    "{lint} under {waiver}"
                );
            }
        }
    }

    #[test]
    fn a_member_the_project_says_nobody_receives_is_not_shipped_code() {
        let declared = [
            "conformance/**".to_string(),
            "benchmark/**".to_string(),
            "crates/outpost-xtask".to_string(),
        ];
        assert!(!project::ships("conformance/fuzz/src/main.rs", &declared));
        assert!(!project::ships("benchmark/src/lib.rs", &declared));
        assert!(!project::ships(
            "crates/outpost-xtask/src/main.rs",
            &declared
        ));
        // The member itself, named without anything under it.
        assert!(!project::ships("crates/outpost-xtask", &declared));
        assert!(project::ships("crates/outpost-core/src/lib.rs", &declared));
        // A prefix that is not a path component: this is a different member.
        assert!(project::ships(
            "crates/outpost-xtask-helper/src/lib.rs",
            &declared
        ));
        assert!(project::ships("src/lib.rs", &[]));
    }

    /// An empty pattern would otherwise prefix-match every path.
    #[test]
    fn an_empty_pattern_excludes_nothing() {
        for pattern in ["", "/", "**", "/**"] {
            assert!(
                project::ships("src/lib.rs", &[pattern.to_string()]),
                "{pattern:?}"
            );
        }
    }

    #[test]
    fn a_member_nobody_receives_still_has_to_say_why_it_suppresses() {
        let src = "#[allow(clippy::unwrap_used)]\nfn f() {}\n";
        let unshipped: Vec<String> = judged("benchmark/src/lib.rs", src, false)
            .unwrap()
            .iter()
            .filter_map(|f| f.item.clone())
            .collect();
        assert_eq!(unshipped, vec![UNREASONED.to_string()]);
        let reasoned = "#[allow(clippy::unwrap_used, reason = \"a bench panics\")]\nfn f() {}\n";
        assert_eq!(
            judged("benchmark/src/lib.rs", reasoned, false).unwrap(),
            Vec::new()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_gate_applies_the_projects_not_shipped_list() {
        let files = [
            ("Cargo.toml", WITH_MEMBER),
            (
                "src/lib.rs",
                "#[allow(clippy::panic, reason = \"deliberate\")]\npub fn f() {}\n",
            ),
            ("benchmark/Cargo.toml", MEMBER),
            (
                "benchmark/src/lib.rs",
                "#[allow(clippy::panic, reason = \"a bench panics\")]\npub fn f() {}\n",
            ),
        ];
        let everything_ships = measure(&ctx_of(&files)).unwrap();
        assert_eq!(
            everything_ships.get(&format!("benchmark/src/lib.rs#{SHIPPED_SAFETY}")),
            Some(1)
        );
        let mut ctx = ctx_of(&files);
        ctx.not_shipped = vec!["benchmark/**".to_string()];
        let series = measure(&ctx).unwrap();
        assert_eq!(
            series.get(&format!("benchmark/src/lib.rs#{SHIPPED_SAFETY}")),
            None
        );
        // What ships is untouched by the declaration.
        assert_eq!(series.get(&format!("src/lib.rs#{SHIPPED_SAFETY}")), Some(1));
    }

    fn rules(path: &str, src: &str) -> Vec<String> {
        faults(path, src)
            .unwrap()
            .iter()
            .filter_map(|f| f.item.clone())
            .collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn source_reads_a_real_tree_and_reports_paths_relative_to_its_root() {
        let ctx = ctx_of(&[
            ("Cargo.toml", MANIFEST),
            ("src/lib.rs", "#![allow(dead_code)]\n"),
            ("docs/sample.rs", "#![allow(dead_code)]\n"),
        ]);
        let series = measure(&ctx).unwrap();
        // cargo does not compile `docs/`, so only the `src/` file counts.
        assert_eq!(series.get(&format!("src/lib.rs#{CRATE_LEVEL}")), Some(1));
        assert_eq!(series.get(&format!("src/lib.rs#{UNREASONED}")), Some(1));
        assert_eq!(series.get(&format!("docs/sample.rs#{CRATE_LEVEL}")), None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_real_tree_with_every_suppression_accounted_for_passes() {
        let ctx = ctx_of(&[
            ("Cargo.toml", MANIFEST),
            (
                "src/lib.rs",
                "#[allow(dead_code, reason = \"the surface is public\")]\npub fn f() {}\n",
            ),
        ]);
        assert_eq!(measure(&ctx).unwrap(), Series::new());
    }

    /// A crate with a suppression in each of two places that sit behind `test-utils`.
    fn gated_helpers(manifest: &str) -> crate::testdir::Held {
        let suppression = "#[allow(clippy::unwrap_used, reason = \"a helper for tests\")]\n\
                           pub fn helper() {}\n";
        let gate = "#[cfg(any(test, feature = \"test-utils\"))]";
        let lib = format!("{gate}\npub mod helpers;\n{gate}\n{suppression}");
        ctx_of(&[
            ("Cargo.toml", manifest),
            ("src/lib.rs", &lib),
            ("src/helpers.rs", suppression),
        ])
    }

    /// Only a dev-dependency enables `test-utils`, so both the item and `helpers` are test code.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_gate_excuses_a_suppression_no_shipped_build_can_reach() {
        let manifest = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
                        [workspace]\n[features]\ntest-utils = []\nsemantic = []\n\
                        [dev-dependencies]\nfixture = { path = \".\", features = [\"test-utils\"] }\n";
        assert_eq!(measure(&gated_helpers(manifest)).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_gate_reports_a_suppression_a_shipped_feature_reaches() {
        let manifest = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
                        [workspace]\n[features]\ndefault = [\"test-utils\"]\ntest-utils = []\n";
        let series = measure(&gated_helpers(manifest)).unwrap();
        assert_eq!(series.get(&format!("src/lib.rs#{SHIPPED_SAFETY}")), Some(1));
        assert_eq!(
            series.get(&format!("src/helpers.rs#{SHIPPED_SAFETY}")),
            Some(1)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_source_file_that_is_not_text_is_stepped_over_rather_than_ending_the_gate() {
        let ctx = ctx_of(&[
            ("Cargo.toml", MANIFEST),
            ("src/lib.rs", "#![allow(dead_code)]\n"),
        ]);
        std::fs::write(ctx.root.join("src/binary.rs"), [0xff, 0xfe, 0x00]).unwrap();
        let series = measure(&ctx).unwrap();
        assert_eq!(series.get(&format!("src/lib.rs#{CRATE_LEVEL}")), Some(1));
        assert_eq!(series.get(&format!("src/binary.rs#{CRATE_LEVEL}")), None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_holding_a_file_the_parser_rejects_reports_that_it_could_not_run() {
        let ctx = ctx_of(&[("Cargo.toml", MANIFEST), ("src/lib.rs", "fn f( {\n")]);
        assert!(measure(&ctx).unwrap_err().starts_with("src/lib.rs: line 1"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_corpus_file_the_parser_rejects_does_not_stop_the_gate_reading_the_crate() {
        let ctx = ctx_of(&[
            ("Cargo.toml", MANIFEST),
            ("src/lib.rs", "#[allow(dead_code)]\nfn f() {}\n"),
            ("tests/corpus/broken.rs", "fn f( {\n"),
        ]);
        let series = measure(&ctx).unwrap();
        assert_eq!(series.get(&format!("src/lib.rs#{UNREASONED}")), Some(1));
        assert_eq!(series.len(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_module_the_integration_tests_share_is_read_where_cargo_compiles_it() {
        let ctx = ctx_of(&[
            ("Cargo.toml", MANIFEST),
            ("src/lib.rs", "mod util;\n"),
            ("src/util.rs", "pub fn u() {}\n"),
            ("tests/it.rs", "mod common;\n#[test]\nfn t() {}\n"),
            ("tests/common/mod.rs", "#![allow(dead_code)]\nmod deeper;\n"),
            ("tests/common/deeper.rs", "#[allow(unused)]\nfn x() {}\n"),
            ("tests/corpus/case.rs", "#[allow(unused)]\nfn y() {}\n"),
        ]);
        let series = measure(&ctx).unwrap();
        assert_eq!(
            [
                "tests/common/mod.rs",
                "tests/common/deeper.rs",
                "tests/corpus/case.rs"
            ]
            .map(|file| series.get(&format!("{file}#{UNREASONED}"))),
            [Some(1), Some(1), None]
        );
    }

    #[test]
    fn a_crate_root_keeps_its_modules_beside_it_and_a_module_keeps_them_below() {
        let here = [String::new()];
        let nested = [String::new(), "crates/a".to_string()];
        assert_eq!(
            [
                modules_beside("tests/it.rs", &here),
                modules_beside("examples/demo.rs", &here),
                modules_beside("benches/speed.rs", &here),
                modules_beside("src/bin/tool.rs", &here),
                modules_beside("build.rs", &here),
                modules_beside("crates/a/tests/it.rs", &nested),
                modules_beside("src/lib.rs", &here),
                modules_beside("src/net.rs", &here),
                modules_beside("tests/common/mod.rs", &here),
            ],
            [
                "tests",
                "examples",
                "benches",
                "src/bin",
                "",
                "crates/a/tests",
                "src",
                "src/net",
                "tests/common",
            ]
        );
    }

    #[test]
    fn every_module_declaration_names_the_files_rustc_would_look_for() {
        let src = "mod a;\nmod inline {}\n#[path = \"x/y.rs\"]\nmod c;\n";
        let here = [String::new()];
        assert_eq!(
            declared(src, "src/lib.rs", &here),
            ["src/a.rs", "src/a/mod.rs", "src/x/y.rs"]
        );
        assert_eq!(
            declared(src, "build.rs", &here),
            ["a.rs", "a/mod.rs", "x/y.rs"]
        );
        // Outside a crate root or `mod.rs`, a `#[path]` is relative to the file's own directory.
        assert_eq!(
            declared(src, "src/step.rs", &here),
            ["src/step/a.rs", "src/step/a/mod.rs", "src/x/y.rs"]
        );
        assert_eq!(
            declared("mod broken {", "src/lib.rs", &here),
            Vec::<String>::new()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri did not end this test in 15 minutes")]
    fn chocks_own_source_trips_no_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let here = Ctx::at(root);
        let mut found = Vec::new();
        for path in sources(&here).unwrap().0 {
            let shown = project::relative(root, &path);
            let src = std::fs::read_to_string(&path).unwrap();
            found.extend(faults(&shown, &src).unwrap().iter().map(Finding::render));
        }
        assert_eq!(found, Vec::<String>::new());
    }
}
