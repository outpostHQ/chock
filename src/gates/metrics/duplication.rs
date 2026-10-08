//! Functions whose bodies are copies, exact or near. Names, literal values, attributes and
//! formatting are erased before bodies are compared.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::mem::discriminant;
use std::ops::Range;

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::Visit;
use syn::{Attribute, Block, Meta, Path, Signature};

use crate::gates::metrics::{complexity, prodlines};
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Details, Gate, Group, Kind, Measurement};

pub const GATE: Gate = Gate {
    name: "duplication",
    about: "a function body copied elsewhere in the tree, exactly or near enough to merge",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        // Keyed by item, so a new copy the baseline never saw trips the gate.
        keys: Keys::Items,
        unit: "duplicate(s)",
    },
};

/// Fewest normalised nodes a body needs to be compared; nodes, so formatting cannot move it.
pub const MINIMUM_NODES: usize = 70;

/// Fewest top-level statements, tail expression included, a body needs to be compared.
pub const MINIMUM_STATEMENTS: usize = 3;

/// Sørensen-Dice similarity, in basis points, at which two shapes count as one. At 7500 the larger
/// body may be 1.67x the smaller.
pub const NEAR_THRESHOLD: u64 = 7_500;

/// One function and the canonical form it is compared on; the baseline keys it by `file#name`.
#[derive(Debug, Clone)]
pub struct Function {
    pub file: String,
    pub name: String,
    pub shape: Shape,
    /// The lines from `fn` to the closing brace, for navigation; never part of a key.
    pub line: u32,
    pub last: u32,
    /// The body's bytes in its file, read again only to say what two copies differ in.
    pub bytes: Range<usize>,
}

/// The canonical form of one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// Identity of the whole form. A 64-bit hash, so equal digests are equal forms up to collision.
    pub digest: u64,
    /// One hash per node, sorted, duplicates kept: the multiset a similarity score is taken over.
    pub nodes: Vec<u64>,
    pub statements: usize,
}

impl Shape {
    #[must_use]
    pub fn size(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the body clears both size floors; a smaller one is mostly the names the form erases.
    #[must_use]
    pub fn worth_comparing(&self) -> bool {
        self.size() >= MINIMUM_NODES && self.statements >= MINIMUM_STATEMENTS
    }
}

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let mut kept = Vec::new();
    let mut sources = BTreeMap::new();
    for (path, lines) in prodlines::measure(ctx)? {
        let shown = project::relative(&ctx.root, &path);
        // Zero production lines means a whole-file test module; fixtures are copies on purpose.
        if lines == 0 || is_fixture(&shown) {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|e| format!("reading {shown}: {e}"))?;
        let found = functions(&source, &shown).map_err(|e| format!("{shown}:{e}"))?;
        kept.extend(found.into_iter().filter(|f| f.shape.worth_comparing()));
        sources.insert(shown, source);
    }
    let families = families(kept);
    Ok(Measurement {
        series: series(&families),
        findings: Vec::new(),
        details: details(&families, &sources),
    })
}

/// For each duplicated function, the other members of its family and how to merge them. Two
/// functions under one key share the first one's detail, as they share its number.
fn details(families: &[Vec<Function>], sources: &BTreeMap<String, String>) -> Details {
    let mut details = Details::new();
    for family in families {
        for (index, member) in family.iter().enumerate() {
            let others: Vec<&Function> = family
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| other)
                .collect();
            let key = format!("{}#{}", member.file, member.name);
            details
                .entry(key)
                .or_insert_with(|| detail(member, &others, sources));
        }
    }
    details
}

fn detail(member: &Function, others: &[&Function], sources: &BTreeMap<String, String>) -> Detail {
    let places = others
        .iter()
        .map(|other| {
            let role = match other.shape.digest == member.shape.digest {
                true => "copy".to_string(),
                false => {
                    let alike = similarity(&member.shape.nodes, &other.shape.nodes) / 100;
                    format!("near copy, {alike}% alike")
                }
            };
            Place::at(&role, &other.file, other.line)
                .through(other.last)
                .item(&other.name)
        })
        .collect();
    Detail {
        line: Some(member.line),
        places,
        fix: Some(merge(member, others, sources)),
    }
}

/// Most differing pairs a fix line names; the rest are counted.
const NAMED_PAIRS: usize = 4;

/// How to make one function of `member` and its copies. An exact copy names what differs, so the
/// parameters of the merged function are known; a near copy differs in shape as well.
fn merge(member: &Function, others: &[&Function], sources: &BTreeMap<String, String>) -> String {
    let saves = member.last.saturating_sub(member.line).saturating_add(1);
    let Some(exact) = others
        .iter()
        .find(|other| other.shape.digest == member.shape.digest)
    else {
        return "move the part they share into one function that each of them calls".to_string();
    };
    let pairs = leaves(member, sources).zip(leaves(exact, sources));
    match pairs.and_then(|(left, right)| differs(&left, &right)) {
        Some(pairs) if pairs.is_empty() => format!(
            "delete this copy and call `{}`; saves about {saves} line(s)",
            exact.name
        ),
        Some(pairs) => format!(
            "keep one function, with what differs as parameters ({}); saves about {saves} line(s)",
            named(&pairs)
        ),
        None => format!("keep one function for both; saves about {saves} line(s)"),
    }
}

fn named(pairs: &[(String, String)]) -> String {
    let shown: Vec<String> = pairs
        .iter()
        .take(NAMED_PAIRS)
        .map(|(here, there)| format!("{here} / {there}"))
        .collect();
    match pairs.len().saturating_sub(NAMED_PAIRS) {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}

/// The names and literal values of a body, in order, read again from its file.
fn leaves(function: &Function, sources: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let text = sources.get(&function.file)?.get(function.bytes.clone())?;
    let mut out = Vec::new();
    flatten(text.parse().ok()?, &mut out);
    Some(out)
}

fn flatten(tokens: TokenStream, out: &mut Vec<String>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => flatten(group.stream(), out),
            TokenTree::Ident(ident) => out.push(ident.to_string()),
            TokenTree::Literal(literal) => out.push(literal.to_string()),
            TokenTree::Punct(_) => {}
        }
    }
}

/// Each distinct pair of names or values where two bodies of one form differ, in order of first
/// use, or `None` where their leaves do not line up one to one.
fn differs(left: &[String], right: &[String]) -> Option<Vec<(String, String)>> {
    if left.len() != right.len() {
        return None;
    }
    let mut pairs = Vec::new();
    for (here, there) in left.iter().zip(right) {
        let pair = (here.clone(), there.clone());
        if here != there && !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    Some(pairs)
}

fn is_fixture(shown: &str) -> bool {
    shown
        .split('/')
        .any(|part| part == "fixtures" || part == "testdata")
}

/// One key per duplicated function, valued by how many others share its body.
fn series(families: &[Vec<Function>]) -> Series {
    let mut series = Series::new();
    for family in families {
        let others = u64::try_from(family.len().saturating_sub(1)).unwrap_or(u64::MAX);
        for member in family {
            let key = format!("{}#{}", member.file, member.name);
            crate::gates::metrics::complexity::keep_worst(&mut series, &key, others);
        }
    }
    series
}

/// Every family of two or more functions sharing a body: exact copies grouped first, then near
/// copies linked between them.
#[must_use]
pub fn families(functions: Vec<Function>) -> Vec<Vec<Function>> {
    let forms = forms(functions);
    let mut linked = Linked::over(forms.len());
    link_near(&forms, &mut linked);
    let mut merged: BTreeMap<usize, Vec<Function>> = BTreeMap::new();
    for (index, members) in forms.into_iter().enumerate() {
        merged
            .entry(linked.root(index))
            .or_default()
            .extend(members);
    }
    merged.into_values().filter(|f| f.len() > 1).collect()
}

/// One entry per canonical form with its exact copies, smallest first so `link_near` can stop at
/// the size bound.
fn forms(functions: Vec<Function>) -> Vec<Vec<Function>> {
    let mut by_digest: BTreeMap<u64, Vec<Function>> = BTreeMap::new();
    for function in functions {
        by_digest
            .entry(function.shape.digest)
            .or_default()
            .push(function);
    }
    let mut forms: Vec<Vec<Function>> = by_digest.into_values().collect();
    for members in &mut forms {
        members.sort_by(|left, right| (&left.file, &left.name).cmp(&(&right.file, &right.name)));
    }
    forms.sort_by_key(|members| {
        members
            .first()
            .map_or((0, 0), |f| (f.shape.size(), f.shape.digest))
    });
    forms
}

/// Links near copies, scoring one representative per exact family.
fn link_near(forms: &[Vec<Function>], linked: &mut Linked) {
    for (index, members) in forms.iter().enumerate() {
        let Some(left) = members.first() else {
            continue;
        };
        let bound = largest_comparable(left.shape.size(), NEAR_THRESHOLD);
        for (other, candidates) in forms.iter().enumerate().skip(index + 1) {
            let Some(right) = candidates.first() else {
                continue;
            };
            if right.shape.size() > bound {
                break;
            }
            if similarity(&left.shape.nodes, &right.shape.nodes) >= NEAR_THRESHOLD {
                linked.join(index, other);
            }
        }
    }
}

/// Sørensen-Dice similarity of two sorted multisets, in basis points, found in one linear merge.
#[must_use]
pub fn similarity(left: &[u64], right: &[u64]) -> u64 {
    let total = u64::try_from(left.len().saturating_add(right.len())).unwrap_or(u64::MAX);
    if total == 0 {
        return 0;
    }
    let mut shared = 0_u64;
    let (mut here, mut there) = (0_usize, 0_usize);
    while let (Some(one), Some(two)) = (left.get(here), right.get(there)) {
        match one.cmp(two) {
            Ordering::Less => here += 1,
            Ordering::Greater => there += 1,
            Ordering::Equal => {
                shared += 1;
                here += 1;
                there += 1;
            }
        }
    }
    shared.saturating_mul(20_000) / total
}

/// Largest node count that can still reach `threshold` against `size`. Dice is bounded above by
/// `2 * min / (min + max)`, so a shape past this cannot reach it whatever it contains.
#[must_use]
pub fn largest_comparable(size: usize, threshold: u64) -> usize {
    if threshold == 0 {
        return usize::MAX;
    }
    let size = u64::try_from(size).unwrap_or(u64::MAX);
    usize::try_from(20_000_u64.saturating_sub(threshold).saturating_mul(size) / threshold)
        .unwrap_or(usize::MAX)
}

/// Single-linkage grouping (union-find), so each form belongs to exactly one family.
struct Linked {
    parent: Vec<usize>,
}

impl Linked {
    fn over(size: usize) -> Self {
        Self {
            parent: (0..size).collect(),
        }
    }

    /// The root of `index`, halving the path on the way so a long chain stays cheap.
    fn root(&mut self, mut index: usize) -> usize {
        while let Some(parent) = self.parent.get(index).copied() {
            if parent == index {
                break;
            }
            let above = self.parent.get(parent).copied().unwrap_or(parent);
            if let Some(slot) = self.parent.get_mut(index) {
                *slot = above;
            }
            index = above;
        }
        index
    }

    fn join(&mut self, left: usize, right: usize) {
        let (left, right) = (self.root(left), self.root(right));
        if let Some(slot) = self.parent.get_mut(left.max(right)) {
            *slot = left.min(right);
        }
    }
}

/// Every production function in `source`, canonicalised; tests repeat their setup on purpose. `file`
/// is only a label; an `Err` reads `line: reason` for the caller to prefix with the path.
pub fn functions(source: &str, file: &str) -> Result<Vec<Function>, String> {
    let parsed = syn::parse_file(source).map_err(|e| {
        format!(
            "{}: {e}",
            u32::try_from(e.span().start().line).unwrap_or(u32::MAX)
        )
    })?;
    Ok(complexity::bodies(&parsed, &gated_to_a_harness)
        .into_iter()
        .filter(|found| !gated_to_a_harness(found.attrs))
        .map(|found| {
            let braces = found.body.brace_token.span;
            Function {
                file: file.to_string(),
                name: found.name,
                shape: shape(found.sig, found.body),
                line: line_of(found.sig.fn_token.span.start().line),
                last: line_of(braces.close().end().line),
                bytes: braces.join().byte_range(),
            }
        })
        .collect())
}

fn line_of(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

/// Attributes, matched by last path segment, that mark a test, bench or proof harness.
const HARNESS_MARKERS: [&str; 4] = ["test", "bench", "should_panic", "proof"];

fn gated_to_a_harness(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| match &attr.meta {
        Meta::List(list) => list.path.is_ident("cfg") && names_test(&list.tokens),
        Meta::Path(path) => path
            .segments
            .last()
            .is_some_and(|last| HARNESS_MARKERS.contains(&last.ident.to_string().as_str())),
        Meta::NameValue(_) => false,
    })
}

/// Whether `test` appears anywhere in a `cfg` predicate, `not(test)` included, so this gate errs
/// towards comparing less.
fn names_test(tokens: &TokenStream) -> bool {
    tokens.clone().into_iter().any(|tree| match tree {
        TokenTree::Ident(name) => name == "test",
        TokenTree::Group(group) => names_test(&group.stream()),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

/// The canonical form of one function, signature included.
#[must_use]
pub fn shape(sig: &Signature, body: &Block) -> Shape {
    let mut form = Form::default();
    form.open(numbered("fn", 0));
    form.visit_signature(sig);
    form.visit_block(body);
    let digest = form.close();
    form.nodes.sort_unstable();
    Shape {
        digest,
        nodes: form.nodes,
        statements: body.stmts.len(),
    }
}

/// The canonical form under construction: one hash per node still open, and the finished ones.
#[derive(Debug, Default)]
struct Form {
    stack: Vec<u64>,
    nodes: Vec<u64>,
    names: HashMap<String, u64>,
}

impl Form {
    fn open(&mut self, tag: u64) {
        self.stack.push(mix(SEED, tag));
    }

    /// Closes the node on top: it joins the multiset, and folds into its parent so that a node's
    /// hash covers everything below it.
    fn close(&mut self) -> u64 {
        let hash = self.stack.pop().unwrap_or(SEED);
        self.nodes.push(hash);
        if let Some(parent) = self.stack.last_mut() {
            *parent = mix(*parent, hash);
        }
        hash
    }

    fn walk(&mut self, tag: u64, into: impl FnOnce(&mut Self)) {
        self.open(tag);
        into(self);
        self.close();
    }

    fn leaf(&mut self, tag: u64) {
        self.walk(tag, |_| {});
    }

    /// Folds a value, such as a `mut` keyword, into the open node without adding a node.
    fn note(&mut self, value: u64) {
        if let Some(top) = self.stack.last_mut() {
            *top = mix(*top, value);
        }
    }

    /// An identifier becomes the position it was first seen at, so renaming a function and all its
    /// locals changes nothing, while `a + a` stays apart from `a + b`.
    fn placeholder(&mut self, name: &str) -> u64 {
        let next = u64::try_from(self.names.len()).unwrap_or(u64::MAX);
        *self.names.entry(name.to_string()).or_insert(next)
    }
}

impl<'ast> Visit<'ast> for Form {
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        self.walk(label("expr", node), |form| {
            syn::visit::visit_expr(form, node);
        });
    }

    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        self.walk(label("stmt", node), |form| {
            syn::visit::visit_stmt(form, node);
        });
    }

    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        self.walk(label("pat", node), |form| {
            syn::visit::visit_pat(form, node);
        });
    }

    fn visit_type(&mut self, node: &'ast syn::Type) {
        self.walk(label("type", node), |form| {
            syn::visit::visit_type(form, node);
        });
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.walk(label("item", node), |form| {
            syn::visit::visit_item(form, node);
        });
    }

    fn visit_member(&mut self, node: &'ast syn::Member) {
        self.walk(label("member", node), |form| {
            syn::visit::visit_member(form, node);
        });
    }

    fn visit_bin_op(&mut self, node: &'ast syn::BinOp) {
        self.leaf(label("binop", node));
    }

    fn visit_un_op(&mut self, node: &'ast syn::UnOp) {
        self.leaf(label("unop", node));
    }

    fn visit_range_limits(&mut self, node: &'ast syn::RangeLimits) {
        self.leaf(label("range", node));
    }

    /// A literal keeps its type and loses its value.
    fn visit_lit(&mut self, node: &'ast syn::Lit) {
        self.leaf(label("lit", node));
    }

    /// A tuple field is a position, not a literal: `.0` and `.1` name different fields.
    fn visit_index(&mut self, node: &'ast syn::Index) {
        self.leaf(numbered("index", u64::from(node.index)));
    }

    fn visit_ident(&mut self, node: &'ast proc_macro2::Ident) {
        let seen = self.placeholder(&node.to_string());
        self.leaf(numbered("name", seen));
    }

    fn visit_lifetime(&mut self, node: &'ast syn::Lifetime) {
        let seen = self.placeholder(&node.ident.to_string());
        self.leaf(numbered("lifetime", seen));
    }

    /// Attributes are ignored, so copies differing only by `#[inline]` still match.
    fn visit_attribute(&mut self, _node: &'ast Attribute) {}

    /// A macro is opaque below its name, which is kept verbatim.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.leaf(hash_text(
            label("macro", &node.delimiter),
            &path_text(&node.path),
        ));
    }

    /// `&mut` is not `&`, in an expression or in a type, and neither is a node of its own.
    fn visit_expr_reference(&mut self, node: &'ast syn::ExprReference) {
        self.note(flags(&[node.mutability.is_some()]));
        syn::visit::visit_expr_reference(self, node);
    }

    fn visit_type_reference(&mut self, node: &'ast syn::TypeReference) {
        self.note(flags(&[node.mutability.is_some()]));
        syn::visit::visit_type_reference(self, node);
    }

    fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
        self.note(flags(&[node.by_ref.is_some(), node.mutability.is_some()]));
        syn::visit::visit_pat_ident(self, node);
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.note(flags(&[
            node.constness.is_some(),
            node.asyncness.is_some(),
            node.capture.is_some(),
        ]));
        syn::visit::visit_expr_closure(self, node);
    }

    fn visit_signature(&mut self, node: &'ast Signature) {
        self.note(flags(&[
            node.constness.is_some(),
            node.asyncness.is_some(),
            matches!(node.safety, syn::Safety::Unsafe(_)),
            matches!(node.safety, syn::Safety::Safe(_)),
            node.variadic.is_some(),
        ]));
        syn::visit::visit_signature(self, node);
    }
}

/// Keyword modifiers packed into one number, behind a leading bit so lists of different lengths
/// never collide.
fn flags(present: &[bool]) -> u64 {
    present
        .iter()
        .fold(1, |bits, set| (bits << 1) | u64::from(*set))
}

fn path_text(path: &Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// A node's label: which syn enum it came from, and which variant of it. The variant is read as a
/// discriminant rather than matched, because `syn::Expr` is `#[non_exhaustive]` and grows.
fn label<T>(family: &str, node: &T) -> u64 {
    let mut hasher = Fnv(hash_text(SEED, family));
    discriminant(node).hash(&mut hasher);
    hasher.0
}

/// A label carrying a number rather than a variant: a tuple index, or where a name was first seen.
fn numbered(family: &str, value: u64) -> u64 {
    mix(hash_text(SEED, family), value)
}

fn hash_text(state: u64, text: &str) -> u64 {
    text.bytes()
        .fold(state, |state, byte| mix(state, u64::from(byte)))
}

/// Seed of the node hashes. Any odd constant works; this one is the FNV offset basis.
const SEED: u64 = 0xcbf2_9ce4_8422_2325;

const fn mix(state: u64, value: u64) -> u64 {
    let mut mixed = (state ^ value).wrapping_mul(0x0100_0000_01b3);
    mixed ^= mixed >> 29;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^ (mixed >> 32)
}

/// A `Hasher` so that a syn discriminant and a raw number mix through the same function.
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0 = bytes
            .iter()
            .fold(self.0, |state, byte| mix(state, u64::from(*byte)));
    }
}

/// Kani proofs over every input within the bound; only `cargo kani` builds them.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Largest node count the proofs cover; a proof says nothing past it.
    const SIZE_BOUND: usize = 64;

    /// The ceiling `largest_comparable` is derived from: two multisets share at most the whole of
    /// the smaller one, and `similarity` scores `shared * 20_000 / total`.
    fn best_possible(smaller: usize, larger: usize) -> u64 {
        let shared = u64::try_from(smaller).unwrap_or(u64::MAX);
        let total = u64::try_from(smaller + larger).unwrap_or(u64::MAX);
        if total == 0 {
            return 0;
        }
        shared.saturating_mul(20_000) / total
    }

    /// `link_near` stops at this bound, so a bound too small would skip a near copy silently.
    #[kani::proof]
    fn a_shape_past_the_bound_could_not_have_reached_the_threshold() {
        let smaller: usize = kani::any();
        let larger: usize = kani::any();
        kani::assume(smaller <= SIZE_BOUND);
        kani::assume(larger <= SIZE_BOUND);
        kani::assume(smaller <= larger);
        let threshold: u64 = kani::any();
        kani::assume(threshold > 0 && threshold <= 10_000);
        kani::assume(larger > largest_comparable(smaller, threshold));
        assert!(best_possible(smaller, larger) < threshold);
    }

    /// The converse: a shape at the bound can still reach the threshold, so the bound is tight.
    #[kani::proof]
    fn the_bound_itself_is_never_ruled_out() {
        let smaller: usize = kani::any();
        kani::assume(smaller > 0 && smaller <= SIZE_BOUND);
        let threshold: u64 = kani::any();
        kani::assume(threshold > 0 && threshold <= 10_000);
        let bound = largest_comparable(smaller, threshold);
        kani::assume(bound <= SIZE_BOUND && bound >= smaller);
        assert!(best_possible(smaller, bound) >= threshold);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::testdir::tree;

    fn found(source: &str) -> Vec<Function> {
        functions(source, "t.rs").unwrap()
    }

    fn digest(source: &str) -> u64 {
        found(source).first().unwrap().shape.digest
    }

    fn names(source: &str) -> Vec<String> {
        found(source).into_iter().map(|f| f.name).collect()
    }

    /// A body over both floors whose names, bound and extra statement the caller chooses.
    fn wide(name: &str, local: &str, bound: &str, extra: &str) -> String {
        format!(
            "fn {name}(rows: &[String], limit: usize) -> Result<Vec<String>, String> {{
                let mut {local} = Vec::new();
                for row in rows {{
                    if row.len() > {bound} {{
                        return Err(format!(\"too long at {{}}\", limit));
                    }}
                    let cell = row.trim().to_string();
                    if cell.is_empty() {{
                        continue;
                    }}
                    {local}.push(cell);
                }}
                {extra}
                {local}.sort();
                {local}.dedup();
                Ok({local})
            }}"
        )
    }

    /// A body over both floors that shares nothing with `wide`.
    fn other(name: &str) -> String {
        format!(
            "fn {name}(text: &str) -> usize {{
                let mut seen = 0;
                let mut chars = text.chars();
                while let Some(letter) = chars.next() {{
                    match letter {{
                        'a' => seen += 2,
                        'b' => seen = seen.saturating_sub(1),
                        'c' => seen *= 3,
                        _ => {{}}
                    }}
                }}
                if seen > 9 {{
                    seen = 9;
                }}
                seen
            }}"
        )
    }

    // Node counts for `sized`: the bare body, each extra statement, and each `+ a` term. A bracket
    // pair adds one node, which lands an exact count.
    const BARE: usize = 9;
    const PER_STATEMENT: usize = 5;
    const PER_TERM: usize = 4;

    /// A body of exactly the node and statement counts asked for.
    fn sized(name: &str, nodes: usize, statements: usize) -> String {
        let extra = statements.saturating_sub(1);
        let over = nodes.saturating_sub(BARE + PER_STATEMENT * extra);
        let lets = "let b = a; ".repeat(extra);
        let terms = " + a".repeat(over / PER_TERM);
        let (open, close) = ("(".repeat(over % PER_TERM), ")".repeat(over % PER_TERM));
        format!("fn {name}(a: u32) {{ {lets}{open}a{close}{terms} }}")
    }

    /// One function with its form given outright, for sizes no real source lands on exactly.
    fn formed(file: &str, name: &str, digest: u64, nodes: usize) -> Function {
        Function {
            file: file.to_string(),
            name: name.to_string(),
            shape: Shape {
                digest,
                nodes: (0..nodes)
                    .map(|n| u64::try_from(n).unwrap_or_default())
                    .collect(),
                statements: MINIMUM_STATEMENTS,
            },
            line: 1,
            last: 1,
            bytes: 0..0,
        }
    }

    fn kept(files: &[(&str, &str)]) -> Vec<Function> {
        files
            .iter()
            .flat_map(|(file, source)| functions(source, file).unwrap())
            .filter(|f| f.shape.worth_comparing())
            .collect()
    }

    /// Every family as baseline keys, sorted so a test compares sets rather than grouping order.
    fn keyed(functions: Vec<Function>) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = families(functions)
            .into_iter()
            .map(|family| {
                family
                    .into_iter()
                    .map(|f| format!("{}#{}", f.file, f.name))
                    .collect()
            })
            .collect();
        out.sort();
        out
    }

    fn grouped(files: &[(&str, &str)]) -> Vec<Vec<String>> {
        keyed(kept(files))
    }

    fn measured(root: &Path) -> Result<Series, String> {
        read(root).map(|read| read.series)
    }

    fn read(root: &Path) -> Result<Measurement, String> {
        measure(&Ctx::at(root))
    }

    #[test]
    fn the_body_the_helper_writes_clears_both_floors_and_the_other_one_shares_nothing_with_it() {
        let shape = found(&wide("f", "out", "3", ""))
            .first()
            .unwrap()
            .shape
            .clone();
        assert!(shape.size() >= MINIMUM_NODES, "{}", shape.size());
        assert!(
            shape.statements >= MINIMUM_STATEMENTS,
            "{}",
            shape.statements
        );
        let apart = found(&other("g")).first().unwrap().shape.clone();
        assert!(apart.size() >= MINIMUM_NODES, "{}", apart.size());
        assert!(similarity(&shape.nodes, &apart.nodes) < NEAR_THRESHOLD);
    }

    #[test]
    fn renaming_a_function_its_parameters_and_its_locals_changes_nothing() {
        assert_eq!(
            digest("fn total(items: &[u32], start: u32) -> u32 { let sum = start; sum + 1 }"),
            digest("fn amount(values: &[u32], base: u32) -> u32 { let count = base; count + 1 }")
        );
    }

    #[test]
    fn a_placeholder_keeps_which_identifier_repeats() {
        assert_ne!(
            digest("fn one(a: u32, b: u32) -> u32 { a + a }"),
            digest("fn two(a: u32, b: u32) -> u32 { a + b }")
        );
    }

    #[test]
    fn a_literal_is_erased_down_to_its_type() {
        assert_eq!(
            digest("fn f() -> u32 { 1 + 2 }"),
            digest("fn f() -> u32 { 40 + 999 }")
        );
        assert_ne!(
            digest("fn f() { let v = \"text\"; }"),
            digest("fn f() { let v = 1; }")
        );
    }

    #[test]
    fn a_literal_and_a_constant_standing_for_it_are_not_the_same_node() {
        assert_ne!(
            digest("fn f(n: usize) -> bool { n > 4 }"),
            digest("fn f(n: usize) -> bool { n > LIMIT }")
        );
    }

    #[test]
    fn comments_and_formatting_never_reach_the_comparison() {
        let spread = "fn f(a: u32) -> u32 {\n    // why\n    let b = a;\n\n    b + 1\n}";
        assert_eq!(
            digest("fn f(a: u32) -> u32 { let b = a; b + 1 }"),
            digest(spread)
        );
    }

    #[test]
    fn an_attribute_is_erased_and_a_bodyless_declaration_is_never_collected() {
        assert_eq!(
            digest("#[inline]\nfn one(a: u32) -> u32 { a }"),
            digest("fn two(b: u32) -> u32 { b }")
        );
        assert!(found("trait T { fn declared(&self); }").is_empty());
    }

    #[test]
    fn a_branch_written_two_ways_stays_two_shapes() {
        assert_ne!(
            digest("fn f(v: u32) -> u32 { if v > 0 { 1 } else { 0 } }"),
            digest("fn f(v: u32) -> u32 { match v { 0 => 0, _ => 1 } }")
        );
    }

    #[test]
    fn two_operators_never_collapse_into_one() {
        assert_ne!(
            digest("fn f(a: u32, b: u32) -> u32 { a + b }"),
            digest("fn f(a: u32, b: u32) -> u32 { a * b }")
        );
    }

    #[test]
    fn a_macro_hides_its_arguments_and_keeps_its_name() {
        assert_eq!(
            digest("fn f() { println!(\"a {}\", 1); }"),
            digest("fn f() { println!(\"b\"); }")
        );
        assert_ne!(
            digest("fn f() { println!(\"a\"); }"),
            digest("fn f() { unreachable!(\"a\"); }")
        );
    }

    #[test]
    fn the_signature_participates_so_two_agreeing_bodies_under_different_ones_stay_apart() {
        assert_ne!(
            digest("fn f(a: u32) -> u32 { 1 }"),
            digest("fn f(a: u32, b: u32) -> u32 { 1 }")
        );
        assert_ne!(
            digest("fn f(a: &u32) -> u32 { 1 }"),
            digest("fn f(a: &mut u32) -> u32 { 1 }")
        );
        assert_ne!(
            digest("fn f() -> u32 { 1 }"),
            digest("async fn f() -> u32 { 1 }")
        );
    }

    #[test]
    fn a_function_defined_inside_another_is_part_of_its_host_rather_than_a_function_itself() {
        assert_eq!(
            names("fn f(a: bool) { fn g(b: bool) { h(b); } g(a); }"),
            vec!["f".to_string()]
        );
    }

    #[test]
    fn a_method_is_found_by_its_own_name_and_so_is_a_trait_method_with_a_body() {
        assert_eq!(
            names("struct S; impl S { fn m(&self) { g(); } }"),
            vec!["m".to_string()]
        );
        assert_eq!(
            names("trait T { fn bare(&self); fn full(&self) { g(); } }"),
            vec!["full".to_string()]
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_a_failure_to_run_rather_than_a_zero() {
        let err = functions("fn f( { this is not rust", "t.rs").unwrap_err();
        assert!(err.starts_with("1: "), "{err}");
    }

    #[test]
    fn the_similarity_of_two_multisets_is_the_dice_score_of_what_they_share() {
        assert_eq!(similarity(&[], &[]), 0);
        assert_eq!(similarity(&[1, 2, 3], &[1, 2, 3]), 10_000);
        assert_eq!(similarity(&[1, 2, 3], &[4, 5, 6]), 0);
        assert_eq!(similarity(&[1, 2, 3, 4], &[1, 2, 3, 9]), 7_500);
        assert_eq!(similarity(&[1, 1, 1], &[1]), 5_000);
    }

    #[test]
    fn the_size_bound_drops_only_the_pairs_that_could_never_reach_the_threshold() {
        assert_eq!(largest_comparable(100, 10_000), 100);
        assert_eq!(largest_comparable(100, 7_500), 166);
        assert_eq!(largest_comparable(100, 0), usize::MAX);
        let (small, large) = (
            (0..100).collect::<Vec<u64>>(),
            (0..167).collect::<Vec<u64>>(),
        );
        assert!(similarity(&small, &large) < 7_500);
    }

    #[test]
    fn a_pair_exactly_at_the_size_bound_is_one_family_and_one_node_past_it_is_not() {
        let small = formed("a.rs", "small", 1, 72);
        let bound = largest_comparable(small.shape.size(), NEAR_THRESHOLD);
        let (at, past) = (
            formed("b.rs", "large", 2, bound),
            formed("b.rs", "large", 2, bound + 1),
        );
        assert_eq!(
            similarity(&small.shape.nodes, &at.shape.nodes),
            NEAR_THRESHOLD
        );
        assert!(similarity(&small.shape.nodes, &past.shape.nodes) < NEAR_THRESHOLD);
        assert_eq!(
            keyed(vec![small.clone(), at]),
            vec![vec!["a.rs#small".to_string(), "b.rs#large".to_string()]]
        );
        assert_eq!(keyed(vec![small, past]), Vec::<Vec<String>>::new());
    }

    #[test]
    fn two_functions_differing_only_in_a_variable_name_are_one_family() {
        let files = [
            ("a.rs", wide("read_rows", "out", "3", "")),
            ("b.rs", wide("load_rows", "kept", "3", "")),
        ];
        let pairs: Vec<(&str, &str)> = files.iter().map(|(f, s)| (*f, s.as_str())).collect();
        assert_eq!(
            grouped(&pairs),
            vec![vec![
                "a.rs#read_rows".to_string(),
                "b.rs#load_rows".to_string()
            ]]
        );
    }

    #[test]
    fn two_functions_differing_only_in_a_literal_are_one_family() {
        let files = [
            ("a.rs", wide("one", "out", "3", "")),
            ("b.rs", wide("two", "out", "4096", "")),
        ];
        let pairs: Vec<(&str, &str)> = files.iter().map(|(f, s)| (*f, s.as_str())).collect();
        assert_eq!(
            grouped(&pairs),
            vec![vec!["a.rs#one".to_string(), "b.rs#two".to_string()]]
        );
    }

    #[test]
    fn two_functions_differing_only_in_a_variable_name_share_one_canonical_form() {
        assert_eq!(
            digest(&wide("read_rows", "out", "3", "")),
            digest(&wide("load_rows", "kept", "3", ""))
        );
    }

    #[test]
    fn a_body_with_one_statement_added_is_a_near_duplicate_rather_than_an_exact_one() {
        let (plain, grown) = (
            wide("one", "out", "3", ""),
            wide("two", "out", "3", "out.reverse();"),
        );
        assert_ne!(digest(&plain), digest(&grown));
        assert_eq!(
            grouped(&[("a.rs", plain.as_str()), ("b.rs", grown.as_str())]),
            vec![vec!["a.rs#one".to_string(), "b.rs#two".to_string()]]
        );
    }

    #[test]
    fn two_functions_doing_different_work_are_never_a_family() {
        let (rows, count) = (wide("one", "out", "3", ""), other("two"));
        assert_eq!(
            grouped(&[("a.rs", rows.as_str()), ("b.rs", count.as_str())]),
            Vec::<Vec<String>>::new()
        );
    }

    #[test]
    fn a_family_names_every_one_of_its_members_once() {
        let copies: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|name| wide(name, "out", "3", ""))
            .collect();
        let files: Vec<(&str, &str)> = vec![
            ("a.rs", copies[0].as_str()),
            ("b.rs", copies[1].as_str()),
            ("c.rs", copies[2].as_str()),
        ];
        assert_eq!(
            grouped(&files),
            vec![vec![
                "a.rs#one".to_string(),
                "b.rs#two".to_string(),
                "c.rs#three".to_string()
            ]]
        );
    }

    #[test]
    fn a_body_sitting_exactly_on_a_floor_is_compared_and_one_step_under_it_is_not() {
        let cases = [
            (MINIMUM_NODES, MINIMUM_STATEMENTS, true),
            (MINIMUM_NODES - 1, MINIMUM_STATEMENTS, false),
            (MINIMUM_NODES, MINIMUM_STATEMENTS - 1, false),
        ];
        for (nodes, statements, compared) in cases {
            let source = sized("f", nodes, statements);
            let shape = found(&source).first().unwrap().shape.clone();
            assert_eq!(
                (shape.size(), shape.statements),
                (nodes, statements),
                "{source}"
            );
            assert_eq!(
                shape.worth_comparing(),
                compared,
                "{nodes} nodes and {statements} statements: {source}"
            );
        }
    }

    #[test]
    fn two_copies_of_a_body_sitting_exactly_on_both_floors_are_one_family() {
        let (one, two) = (
            sized("one", MINIMUM_NODES, MINIMUM_STATEMENTS),
            sized("two", MINIMUM_NODES, MINIMUM_STATEMENTS),
        );
        assert_eq!(
            grouped(&[("a.rs", one.as_str()), ("b.rs", two.as_str())]),
            vec![vec!["a.rs#one".to_string(), "b.rs#two".to_string()]]
        );
    }

    #[test]
    fn a_body_two_functions_share_below_the_node_floor_is_never_reported() {
        let source = "fn one(a: u32) -> u32 { let b = a; let c = b + 1; c * 2 }";
        let twin = "fn two(x: u32) -> u32 { let y = x; let z = y + 1; z * 2 }";
        assert_eq!(digest(source), digest(twin));
        assert!(!found(source).first().unwrap().shape.worth_comparing());
        assert_eq!(
            grouped(&[("a.rs", source), ("b.rs", twin)]),
            Vec::<Vec<String>>::new()
        );
    }

    #[test]
    fn a_two_statement_body_is_below_the_floor_however_many_nodes_it_has() {
        let source = "fn f(text: &str) -> String {
            let trimmed = text.trim().to_lowercase()
                .replace('a', \"b\").replace('c', \"d\")
                .replace('e', \"f\").replace('g', \"h\");
            trimmed.chars().rev()
                .filter(|c| c.is_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .take(9)
                .collect::<String>()
        }";
        let shape = found(source).first().unwrap().shape.clone();
        assert!(shape.size() >= MINIMUM_NODES, "{}", shape.size());
        assert_eq!(shape.statements, 2);
        assert!(!shape.worth_comparing());
    }

    #[test]
    fn two_lists_sharing_nodes_score_by_how_many_they_share() {
        assert_eq!(similarity(&[1, 2, 3], &[1, 2, 3]), 10_000);
        assert_eq!(similarity(&[1, 2], &[1, 3]), 5_000);
        assert_eq!(similarity(&[1, 2], &[3, 4]), 0);
    }

    /// The control for the tests that a gated module is skipped.
    #[test]
    fn a_function_inside_an_ordinary_module_is_compared_like_any_other() {
        let body = wide("one", "out", "3", "");
        assert_eq!(names(&format!("mod inner {{ {body} }}")), ["one"]);
    }

    #[test]
    fn a_function_inside_a_cfg_test_module_is_never_compared() {
        let body = wide("one", "out", "3", "");
        let source = format!("#[cfg(test)]\nmod tests {{ {body} }}");
        assert_eq!(names(&source), Vec::<String>::new());
    }

    #[test]
    fn a_test_function_in_a_file_with_no_gate_of_its_own_is_never_compared() {
        let body = wide("one", "out", "3", "");
        assert_eq!(names(&format!("#[test]\n{body}")), Vec::<String>::new());
        assert_eq!(
            names(&format!("#[tokio::test]\n{body}")),
            Vec::<String>::new()
        );
        assert_eq!(
            names(&format!("#[cfg(test)]\n{body}")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_kani_harness_is_a_harness_and_is_never_compared() {
        let body = wide("one", "out", "3", "");
        assert_eq!(
            names(&format!("#[kani::proof]\n{body}")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cfg_that_names_no_test_leaves_its_function_where_it_is() {
        let body = wide("one", "out", "3", "");
        assert_eq!(
            names(&format!("#[cfg(feature = \"test\")]\n{body}")),
            vec!["one".to_string()]
        );
        assert_eq!(
            names(&format!("#[cfg(unix)]\n{body}")),
            vec!["one".to_string()]
        );
    }

    #[test]
    fn a_directory_of_tool_inputs_is_not_code_anyone_maintains() {
        assert!(is_fixture("tests/fixtures/one.rs"));
        assert!(is_fixture("crates/a/fixtures/lenses/rust/one.rs"));
        assert!(is_fixture("src/testdata/one.rs"));
        assert!(!is_fixture("src/fixtures.rs"));
        assert!(!is_fixture("src/gates/duplication.rs"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_series_gives_every_member_of_a_family_how_many_others_share_its_body() {
        let copies: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|name| wide(name, "out", "3", ""))
            .collect();
        let dir = tree(
            "duplication-family",
            &[
                ("src/a.rs", copies[0].as_str()),
                ("src/b.rs", copies[1].as_str()),
                ("src/c.rs", copies[2].as_str()),
            ],
        );
        let series = measured(&dir).unwrap();
        assert_eq!(series.get("src/a.rs#one"), Some(2));
        assert_eq!(series.get("src/b.rs#two"), Some(2));
        assert_eq!(series.get("src/c.rs#three"), Some(2));
        assert_eq!(series.len(), 3);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn two_functions_of_the_same_name_in_one_file_keep_the_worst() {
        let shared = format!(
            "struct A; impl A {{ {} }} struct B; impl B {{ {} }}",
            wide("parse", "out", "3", ""),
            other("parse")
        );
        let dir = tree(
            "duplication-same-name",
            &[
                ("src/a.rs", shared.as_str()),
                ("src/b.rs", &wide("copy", "out", "3", "")),
                ("src/c.rs", &wide("again", "out", "3", "")),
                ("src/d.rs", &other("twin")),
            ],
        );
        let series = measured(&dir).unwrap();
        assert_eq!(series.get("src/a.rs#parse"), Some(2));
        assert_eq!(series.get("src/d.rs#twin"), Some(1));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_nothing_copied_measures_empty_rather_than_failing() {
        let (one, two) = (wide("one", "out", "3", ""), other("two"));
        let files = [("src/a.rs", one.as_str()), ("src/b.rs", two.as_str())];
        let dir = tree("duplication-clean", &files);
        assert_eq!(measured(&dir).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_copy_under_a_test_module_or_a_fixture_directory_never_reaches_the_series() {
        let body = wide("one", "out", "3", "");
        let dir = tree(
            "duplication-skipped",
            &[
                ("src/a.rs", body.as_str()),
                ("src/b.rs", &format!("#[cfg(test)]\nmod tests {{ {body} }}")),
                ("src/fixtures/c.rs", body.as_str()),
            ],
        );
        assert_eq!(measured(&dir).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_walk_passes_over_leaves_the_files_sorted_after_it_to_be_read() {
        let body = wide("one", "out", "3", "");
        let twin = wide("two", "kept", "3", "");
        let dir = tree(
            "duplication-skip-order",
            &[
                ("src/fixtures/copy.rs", body.as_str()),
                ("src/gated.rs", &format!("#![cfg(test)]\n{body}")),
                ("src/one.rs", body.as_str()),
                ("src/two.rs", twin.as_str()),
            ],
        );
        let series = measured(&dir).unwrap();
        assert_eq!(series.get("src/one.rs#one"), Some(1));
        assert_eq!(series.get("src/two.rs#two"), Some(1));
        assert_eq!(series.get("src/fixtures/copy.rs#one"), None);
        assert_eq!(series.get("src/gated.rs#one"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_parser_rejects_stops_the_measurement_rather_than_reading_as_clean() {
        let dir = tree("duplication-broken", &crate::testdir::UNPARSABLE);
        let err = measured(&dir).unwrap_err();
        assert!(err.contains("src/lib.rs"), "{err}");
    }

    /// What a finding on `key` carries: its line, the other copies, and the fix.
    fn detail_of(files: &[(&str, &str)], key: &str) -> Detail {
        let dir = tree("duplication-detail", files);
        read(&dir).unwrap().details.remove(key).unwrap()
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_exact_copy_names_the_other_place_and_each_name_and_value_that_differs() {
        let (left, right) = (wide("left", "out", "3", ""), wide("right", "kept", "5", ""));
        let found = detail_of(
            &[("src/a.rs", left.as_str()), ("src/b.rs", right.as_str())],
            "src/a.rs#left",
        );
        assert_eq!(found.line, Some(1));
        let copy = Place::at("copy", "src/b.rs", 1).through(17).item("right");
        assert_eq!(found.places, [copy]);
        assert_eq!(
            found.fix.as_deref(),
            Some(
                "keep one function, with what differs as parameters (out / kept, 3 / 5); saves about 17 line(s)"
            )
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_copy_that_differs_in_nothing_is_deleted_for_a_call_to_the_other() {
        let (one, two) = (wide("one", "out", "3", ""), wide("two", "out", "3", ""));
        let found = detail_of(
            &[("src/a.rs", one.as_str()), ("src/b.rs", two.as_str())],
            "src/b.rs#two",
        );
        assert_eq!(
            found.fix.as_deref(),
            Some("delete this copy and call `one`; saves about 17 line(s)")
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_near_copy_says_how_alike_it_is_and_asks_for_the_shared_part_alone() {
        let (plain, grown) = (
            wide("one", "out", "3", ""),
            wide("two", "out", "3", "out.reverse();"),
        );
        let found = detail_of(
            &[("src/a.rs", plain.as_str()), ("src/b.rs", grown.as_str())],
            "src/a.rs#one",
        );
        let alike = similarity(&found_shape(&plain), &found_shape(&grown)) / 100;
        assert_eq!(found.places[0].role, format!("near copy, {alike}% alike"));
        assert_eq!(
            found.fix.as_deref(),
            Some("move the part they share into one function that each of them calls")
        );
    }

    fn found_shape(source: &str) -> Vec<u64> {
        found(source).remove(0).shape.nodes
    }

    #[test]
    fn copies_whose_names_do_not_line_up_still_get_one_function_to_keep() {
        let (here, there) = (formed("a.rs", "f", 7, 80), formed("b.rs", "g", 7, 80));
        assert_eq!(
            merge(&here, &[&there], &BTreeMap::new()),
            "keep one function for both; saves about 1 line(s)"
        );
        let short = ["a".to_string()];
        assert_eq!(differs(&short, &[]), None);
    }

    #[test]
    fn an_exact_copy_is_a_call_where_its_leaves_match_and_takes_parameters_where_they_differ() {
        let sources = BTreeMap::from([
            ("a.rs".to_string(), "x + 1".to_string()),
            ("b.rs".to_string(), "x + 1".to_string()),
            ("c.rs".to_string(), "y + 2".to_string()),
        ]);
        let body = |file: &str, name: &str| Function {
            bytes: 0..5,
            ..formed(file, name, 7, 80)
        };
        let (here, same, other) = (body("a.rs", "f"), body("b.rs", "g"), body("c.rs", "h"));
        assert_eq!(
            merge(&here, &[&same], &sources),
            "delete this copy and call `g`; saves about 1 line(s)"
        );
        assert_eq!(
            merge(&here, &[&other], &sources),
            "keep one function, with what differs as parameters (x / y, 1 / 2); saves about 1 line(s)"
        );
    }

    #[test]
    fn each_differing_pair_is_named_once_in_order_of_first_use() {
        let leaves = |words: &[&str]| words.iter().map(ToString::to_string).collect::<Vec<_>>();
        let pair = |here: &str, there: &str| (here.to_string(), there.to_string());
        assert_eq!(
            differs(
                &leaves(&["a", "x", "a", "y"]),
                &leaves(&["b", "x", "b", "z"])
            ),
            Some(vec![pair("a", "b"), pair("y", "z")])
        );
    }

    #[test]
    fn a_long_list_of_differences_names_the_first_four_and_counts_the_rest() {
        let pairs: Vec<(String, String)> =
            (0..6).map(|n| (format!("a{n}"), format!("b{n}"))).collect();
        assert_eq!(
            named(&pairs),
            "a0 / b0, a1 / b1, a2 / b2, a3 / b3, and 2 more"
        );
        assert_eq!(named(&pairs[..1]), "a0 / b0");
    }

    #[test]
    fn the_gate_is_a_ratchet_over_items_counted_in_duplicates() {
        assert_eq!(GATE.name, "duplication");
        assert_eq!(crate::run::rerun(GATE.name), "chock run duplication");
        assert_eq!(GATE.kind.ratcheted(), Some((Keys::Items, "duplicate(s)")));
    }
}
