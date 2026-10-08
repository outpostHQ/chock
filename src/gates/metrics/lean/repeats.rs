//! Runs of statements or match arms that repeat in one shape across the production files, and the
//! lines that one function, one table or a call to a function already written would remove.

use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};

use proc_macro2::{Delimiter, Spacing, Span, TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Block, Expr, ExprBreak, ExprContinue, ExprMatch, Ident, ImplItemFn, Item, ItemFn,
    ItemImpl, ItemMod, Macro, PatIdent, Stmt, TraitItemFn,
};

use crate::gates::metrics::prodlines;
use crate::run::report::Place;

/// The fewest lines a copy spans for its run to be worth a name of its own.
const LINES: u32 = 3;
/// The fewest tokens in a copy, so a few lines of punctuation never count.
const TOKENS: usize = 30;
/// The most names or values that may differ between copies: each one is a parameter or a column.
const VALUES: usize = 4;
/// The fewest lines a merge must remove to be named. A judgment: below it, the jump a reader makes
/// to the shared code costs more than the lines it saves.
const SAVES: i64 = 6;
/// The most distinct values a fix names for one parameter.
const SHOWN: usize = 4;
/// Marks a string a macro reads, such as a format string. A function cannot take one as a
/// parameter, so copies that differ in one do not merge.
const FORMAT: char = '\u{1}';

/// Words a shape keeps as written: keywords, `true` and `false`, and the variants of `Option` and
/// `Result`. Every other name is a value that may differ.
const KEPT: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield", "Some", "None", "Ok", "Err",
];

/// How a group of copies merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// Into one new function that each copy calls.
    Function,
    /// Side by side in one block or match: into one table of rows, and the shared lines once.
    Table,
    /// Into a call to the function whose whole body is one of the copies.
    Call(String),
}

/// The lines one copy spans.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Site {
    pub file: String,
    pub line: u32,
    pub last: u32,
}

/// A group of copies in path and line order, how they merge, the lines the merge removes, and
/// each tuple of names or values that differs between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repeat {
    pub sites: Vec<Site>,
    pub merge: Merge,
    pub saves: u32,
    pub differs: Vec<String>,
}

impl Repeat {
    /// Each copy's part of the lines the merge removes: an even split, and the rest to the first.
    pub fn shares(&self) -> Vec<u64> {
        let copies = u32::try_from(self.sites.len()).unwrap_or(u32::MAX).max(1);
        let rest = self.saves % copies;
        (0..copies)
            .map(|at| u64::from(self.saves / copies + if at == 0 { rest } else { 0 }))
            .collect()
    }

    /// Each copy as a place, named as a copy or a row of the group.
    pub fn places(&self) -> Vec<Place> {
        let role = if self.merge == Merge::Table {
            "row"
        } else {
            "copy"
        };
        let copies = self.sites.len();
        (self.sites.iter().enumerate())
            .map(|(at, site)| {
                let named = format!("{role} {} of {copies}", at + 1);
                Place::at(&named, &site.file, site.line).through(site.last)
            })
            .collect()
    }

    /// What to do, in words.
    pub fn fix(&self) -> String {
        let copies = self.sites.len();
        let saves = self.saves;
        let (parameter, passing, values) = match self.differs.as_slice() {
            [] => ("", "", String::new()),
            [one] => (
                " with a parameter",
                ", passing a value",
                format!(" for {one}"),
            ),
            differs => (
                " with parameters",
                ", passing values",
                format!(" for {}", differs.join(", ")),
            ),
        };
        match &self.merge {
            Merge::Function => format!(
                "move the {copies} copies into one function{parameter}{values}, and call it from \
                 each copy; that removes {saves} lines"
            ),
            Merge::Table => format!(
                "write the {copies} rows once, as a loop or one arm over a table{values}; that \
                 removes {saves} lines"
            ),
            Merge::Call(name) => format!(
                "call `{name}` in place of each other copy{passing}{values}; that removes {saves} \
                 lines"
            ),
        }
    }
}

/// A block or a match that holds units: its file, its first unit, how many it holds, and the
/// function's name where it is a whole function body.
#[derive(Debug)]
struct Holder {
    file: usize,
    first: usize,
    len: usize,
    body: Option<String>,
}

/// One statement or arm: its lines, its shape's symbol, its token count, its names and values in
/// order, the names it binds anywhere, and the names a `let` binds for the lines after it.
#[derive(Debug)]
struct Unit {
    holder: usize,
    line: u32,
    last: u32,
    symbol: Option<u64>,
    tokens: usize,
    leaves: Vec<u32>,
    bound: Vec<u32>,
    lets: Vec<u32>,
}

/// The statements and arms of every production file read so far, each name kept once.
#[derive(Debug, Default)]
pub struct Corpus {
    files: Vec<String>,
    holders: Vec<Holder>,
    units: Vec<Unit>,
    names: HashMap<String, u32>,
    texts: Vec<String>,
}

impl Corpus {
    /// Reads one file's production blocks and matches. A file that does not parse, or that only
    /// builds for tests, adds nothing.
    pub fn add(&mut self, shown: &str, src: &str) {
        let Ok(file) = prodlines::parse_rust(src) else {
            return;
        };
        if !prodlines::is_test_gated(&file.attrs) {
            self.files.push(shown.to_string());
            Walk {
                corpus: self,
                body: None,
            }
            .visit_file(&file);
        }
    }

    /// The groups to name, most lines removed first; no two share a line.
    pub fn repeats(&self) -> Vec<Repeat> {
        let (symbols, units) = self.symbols();
        let order = suffixes(&symbols);
        let shared = common(&symbols, &order);
        let found = intervals(&order, &shared)
            .into_iter()
            .filter_map(|(length, mut starts)| {
                let before: BTreeSet<Option<u64>> = starts
                    .iter()
                    .map(|at| at.checked_sub(1).map(|it| symbols[it]))
                    .collect();
                if before.len() == 1 && !before.contains(&None) {
                    return None;
                }
                starts.sort_unstable();
                let mut apart: Vec<usize> = Vec::new();
                for at in starts {
                    if apart.last().is_none_or(|last| at >= last + length) {
                        apart.push(at);
                    }
                }
                let tandem = apart.windows(2).all(|pair| pair[1] == pair[0] + length);
                let firsts: Vec<usize> = apart.iter().filter_map(|&at| units[at]).collect();
                self.judge(&firsts, length, tandem)
            })
            .collect();
        settle(found)
    }

    /// One symbol per unit, holder after holder, then one of its own after each holder, so that no
    /// run crosses into the next. A unit that may not move is a symbol of its own too.
    fn symbols(&self) -> (Vec<u64>, Vec<Option<usize>>) {
        let mut symbols = Vec::with_capacity(self.units.len() + self.holders.len());
        let mut units = Vec::with_capacity(symbols.capacity());
        let mut own = 1_u64 << 63;
        for holder in &self.holders {
            for at in holder.first..holder.first + holder.len {
                symbols.push(self.units[at].symbol.unwrap_or_else(|| {
                    own += 1;
                    own
                }));
                units.push(Some(at));
            }
            own += 1;
            symbols.push(own);
            units.push(None);
        }
        (symbols, units)
    }

    /// The group whose copies start at `firsts` and run `length` units, if merging it removes
    /// enough lines and nothing stands in the way.
    fn judge(&self, firsts: &[usize], length: usize, tandem: bool) -> Option<Repeat> {
        let runs: Vec<&[Unit]> = firsts
            .iter()
            .map(|&first| &self.units[first..first + length])
            .collect();
        let lines: Vec<u32> = runs
            .iter()
            .map(|run| (run[length - 1].last + 1).saturating_sub(run[0].line))
            .collect();
        let tokens: usize = runs.first()?.iter().map(|unit| unit.tokens).sum();
        let least = *lines.iter().min()?;
        let whole: Vec<Option<&str>> = firsts
            .iter()
            .map(|&first| self.whole(first, length))
            .collect();
        let outlive = firsts
            .iter()
            .map(|&first| self.outliving(first, length))
            .max()?;
        if runs.len() < 2 || tokens < TOKENS || least < LINES || outlive > 1 {
            return None;
        }
        let differs = self.differs(&runs)?;
        let removed: i64 = lines.iter().map(|&it| i64::from(it) - 1).sum();
        let bodies: i64 = (lines.iter().zip(&whole))
            .filter_map(|(&it, whole)| whole.map(|_| i64::from(it) - 1))
            .sum();
        let (merge, saves) = match whole.iter().flatten().next() {
            _ if whole.iter().all(Option::is_some) || differs.len() > VALUES => return None,
            _ if tandem => {
                let rows = i64::try_from(runs.len()).ok()?;
                (Merge::Table, removed + rows - (i64::from(least) + rows + 2))
            }
            Some(name) => (Merge::Call((*name).to_string()), removed - bodies),
            None => {
                let values = i64::try_from(differs.len()).ok()?;
                (Merge::Function, removed - (i64::from(least) + 2 + values))
            }
        };
        if saves < SAVES {
            return None;
        }
        let mut sites: Vec<Site> = runs
            .iter()
            .map(|run| Site {
                file: self.files[self.holders[run[0].holder].file].clone(),
                line: run[0].line,
                last: run[length - 1].last,
            })
            .collect();
        sites.sort();
        let differs = differs.iter().map(|tuple| self.shown(tuple)).collect();
        let saves = u32::try_from(saves).ok()?;
        Some(Repeat {
            sites,
            merge,
            saves,
            differs,
        })
    }

    /// The function whose whole body is the run of `length` units from `first`, where one is.
    fn whole(&self, first: usize, length: usize) -> Option<&str> {
        let holder = &self.holders[self.units[first].holder];
        if holder.first == first && holder.len == length {
            holder.body.as_deref()
        } else {
            None
        }
    }

    /// How many names the `let`s of a run bind that the rest of its block still reads. The first
    /// statement after the run that names one decides: it reads the name, or only binds it again.
    fn outliving(&self, first: usize, length: usize) -> usize {
        let holder = &self.holders[self.units[first].holder];
        let after = &self.units[first + length..holder.first + holder.len];
        let lets: BTreeSet<u32> = self.units[first..first + length]
            .iter()
            .flat_map(|unit| unit.lets.iter().copied())
            .collect();
        let read = |name: u32| {
            after.iter().find_map(|unit| {
                let named = unit.leaves.iter().filter(|&&it| it == name).count();
                (named > 0).then(|| named > usize::from(unit.lets.contains(&name)))
            })
        };
        lets.into_iter()
            .filter(|&name| read(name) == Some(true))
            .count()
    }

    /// Each distinct tuple of values that differs across the copies, leaving out names each copy
    /// binds; none where a macro's string differs or the bound names stand in other places.
    fn differs(&self, runs: &[&[Unit]]) -> Option<Vec<Vec<u32>>> {
        let leaves: Vec<Vec<u32>> = runs
            .iter()
            .map(|run| run.iter().flat_map(|unit| unit.leaves.clone()).collect())
            .collect();
        let bound: Vec<BTreeSet<u32>> = runs
            .iter()
            .map(|run| run.iter().flat_map(|unit| unit.bound.clone()).collect())
            .collect();
        let width = leaves.iter().map(Vec::len).min().unwrap_or(0);
        let mut out: Vec<Vec<u32>> = Vec::new();
        let mut pairs = vec![HashMap::new(); runs.len()];
        for at in 0..width {
            let tuple: Vec<u32> = leaves.iter().map(|copy| copy[at]).collect();
            let same = tuple.iter().all(|it| *it == tuple[0]);
            let own = tuple
                .iter()
                .zip(&bound)
                .all(|(name, names)| names.contains(name));
            if own && !renamed(&tuple, &mut pairs) {
                return None;
            }
            if !same
                && tuple
                    .iter()
                    .any(|&id| self.texts[id as usize].starts_with(FORMAT))
            {
                return None;
            }
            if !same && !own && !out.contains(&tuple) {
                out.push(tuple);
            }
        }
        Some(out)
    }

    /// A tuple as `a` / `b`: each value once, the first few, each cut to a readable length.
    fn shown(&self, tuple: &[u32]) -> String {
        let mut distinct: Vec<u32> = Vec::new();
        for &id in tuple {
            if !distinct.contains(&id) {
                distinct.push(id);
            }
        }
        let values: Vec<String> = (distinct.iter().take(SHOWN))
            .map(|&id| {
                let text = &self.texts[id as usize];
                match text.char_indices().nth(24) {
                    Some((cut, _)) => format!("`{}…`", &text[..cut]),
                    None => format!("`{text}`"),
                }
            })
            .collect();
        match distinct.len().checked_sub(SHOWN) {
            Some(more @ 1..) => format!("{} and {more} more", values.join(" / ")),
            _ => values.join(" / "),
        }
    }

    /// Each name's number, kept once for the whole corpus.
    fn ids(&mut self, names: Vec<String>) -> Vec<u32> {
        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            let next = u32::try_from(self.texts.len()).unwrap_or(u32::MAX);
            let texts = &mut self.texts;
            ids.push(*self.names.entry(name).or_insert_with_key(|name| {
                texts.push(name.clone());
                next
            }));
        }
        ids
    }

    /// Adds a holder of `parts`: each one's span, what it binds and whether it leaves, and the
    /// names a `let` binds.
    fn hold(&mut self, body: Option<String>, parts: Vec<(Span, Facts, Vec<String>)>) {
        let holder = self.holders.len();
        self.holders.push(Holder {
            file: self.files.len() - 1,
            first: self.units.len(),
            len: parts.len(),
            body,
        });
        for (span, facts, lets) in parts {
            let mut hasher = DefaultHasher::new();
            let mut leaves = Vec::new();
            let read = span.source_text().and_then(|text| text.parse().ok());
            let tokens = read.map_or(0, |tokens| shape(tokens, false, &mut hasher, &mut leaves));
            let unit = Unit {
                holder,
                line: u32::try_from(span.start().line).unwrap_or(u32::MAX),
                last: u32::try_from(span.end().line).unwrap_or(u32::MAX),
                symbol: (tokens > 0 && !facts.escapes).then(|| hasher.finish() >> 1),
                tokens,
                leaves: self.ids(leaves),
                bound: self.ids(facts.bound),
                lets: self.ids(lets),
            };
            self.units.push(unit);
        }
    }
}

/// Keeps the group that removes most lines first, then each that shares no line with one kept.
fn settle(mut found: Vec<Repeat>) -> Vec<Repeat> {
    found.sort_by(|a, b| b.saves.cmp(&a.saves).then_with(|| a.sites.cmp(&b.sites)));
    let mut taken: Vec<Site> = Vec::new();
    found.retain(|repeat| {
        let free = !repeat.sites.iter().any(|site| {
            taken
                .iter()
                .any(|it| it.file == site.file && it.line <= site.last && site.line <= it.last)
        });
        if free {
            taken.extend(repeat.sites.iter().cloned());
        }
        free
    });
    found
}

/// Feeds the shape of `tokens` to `hasher` and their values to `leaves`, and counts the tokens. A
/// called method or macro stays as written; a string a macro reads is marked, without its names.
fn shape(
    tokens: TokenStream,
    quoted: bool,
    hasher: &mut DefaultHasher,
    leaves: &mut Vec<String>,
) -> usize {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let mut count = tokens.len();
    for (at, token) in tokens.iter().enumerate() {
        let called = matches!(tokens.get(at + 1), Some(TokenTree::Punct(next))
            if next.as_char() == '!' && next.spacing() == Spacing::Alone);
        let method = at.checked_sub(1).is_some_and(
            |before| matches!(&tokens[before], TokenTree::Punct(dot) if dot.as_char() == '.'),
        ) && match tokens.get(at + 1) {
            Some(TokenTree::Group(args)) => args.delimiter() == Delimiter::Parenthesis,
            Some(TokenTree::Punct(next)) => next.as_char() == ':',
            _ => false,
        };
        match token {
            TokenTree::Group(group) => {
                let invoked = at.checked_sub(2).is_some_and(|name| {
                    matches!(&tokens[name..at], [TokenTree::Ident(_), TokenTree::Punct(bang)]
                        if bang.as_char() == '!')
                });
                format!("{:?}", group.delimiter()).hash(hasher);
                count += shape(group.stream(), quoted || invoked, hasher, leaves);
                0_u8.hash(hasher);
            }
            TokenTree::Ident(ident) if called || method || KEPT.iter().any(|it| ident == it) => {
                ident.to_string().hash(hasher);
            }
            TokenTree::Ident(ident) => {
                '_'.hash(hasher);
                leaves.push(ident.to_string());
            }
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                let first = text.chars().next();
                first
                    .filter(|it| !it.is_ascii_digit())
                    .unwrap_or('0')
                    .hash(hasher);
                let string = text.ends_with(['"', '#']);
                leaves.push(if quoted && string {
                    format!("{FORMAT}{}", unnamed(&text))
                } else {
                    text
                });
            }
            TokenTree::Punct(punct) => punct.as_char().hash(hasher),
        }
    }
    count
}

/// Whether each copy's own name at one place pairs with the first copy's as at every place before,
/// both ways, as a rename would leave them. `pairs` holds each copy's pairs.
fn renamed(tuple: &[u32], pairs: &mut [HashMap<(bool, u32), u32>]) -> bool {
    tuple.iter().zip(pairs).all(|(&name, pairs)| {
        *pairs.entry((true, tuple[0])).or_insert(name) == name
            && *pairs.entry((false, name)).or_insert(tuple[0]) == tuple[0]
    })
}

/// A format string without the names it reads inline, so `"{told}: {next}"` and
/// `"{reason}: {under}"` read as one.
fn unnamed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(it) = chars.next() {
        out.push(it);
        if it == '{' && chars.next_if_eq(&'{').is_none() {
            while chars
                .next_if(|next| next.is_alphanumeric() || *next == '_')
                .is_some()
            {}
        }
    }
    out
}

/// Whether a macro's tokens name `return`, `break` or `continue`.
fn jumps(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Group(group) => jumps(group.stream()),
        TokenTree::Ident(ident) => ident == "return" || ident == "break" || ident == "continue",
        _ => false,
    })
}

/// The start of every suffix of `symbols` in sorted order, by prefix doubling.
fn suffixes(symbols: &[u64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..symbols.len()).collect();
    let mut rank = symbols.to_vec();
    let mut width = 1;
    loop {
        let key = |at: usize| (rank[at], rank.get(at + width).map_or(0, |next| next + 1));
        order.sort_unstable_by_key(|&at| key(at));
        let mut next = vec![0_u64; symbols.len()];
        for pair in order.windows(2) {
            next[pair[1]] = next[pair[0]] + u64::from(key(pair[0]) != key(pair[1]));
        }
        let distinct = order.last().map_or(0, |&last| next[last] + 1);
        rank = next;
        if usize::try_from(distinct).is_ok_and(|it| it == symbols.len()) || width >= symbols.len() {
            return order;
        }
        width *= 2;
    }
}

/// For each suffix in sorted order, how many symbols it shares with the one before, by Kasai's
/// method.
fn common(symbols: &[u64], order: &[usize]) -> Vec<usize> {
    let mut rank = vec![0; symbols.len()];
    for (at, &start) in order.iter().enumerate() {
        rank[start] = at;
    }
    let mut shared = vec![0; symbols.len()];
    let mut run = 0;
    for start in 0..symbols.len() {
        let Some(before) = rank[start].checked_sub(1).map(|it| order[it]) else {
            run = 0;
            continue;
        };
        while symbols
            .get(start + run)
            .is_some_and(|it| symbols.get(before + run) == Some(it))
        {
            run += 1;
        }
        shared[rank[start]] = run;
        run = run.saturating_sub(1);
    }
    shared
}

/// Each run that two or more suffixes share and cannot extend to the right: its length, and the
/// start of each copy.
fn intervals(order: &[usize], shared: &[usize]) -> Vec<(usize, Vec<usize>)> {
    let mut out = Vec::new();
    let mut open: Vec<(usize, usize)> = vec![(0, 0)];
    for at in 1..=order.len() {
        let length = shared.get(at).copied().unwrap_or(0);
        let mut first = at - 1;
        while let Some(&(top, from)) = open.last()
            && length < top
        {
            open.pop();
            out.push((top, order[from..at].to_vec()));
            first = from;
        }
        if open.last().is_none_or(|&(top, _)| length > top) {
            open.push((length, first));
        }
    }
    out
}

/// Walks one file's production items and hands each block's statements and each match's arms to
/// the corpus.
struct Walk<'a> {
    corpus: &'a mut Corpus,
    body: Option<String>,
}

impl Walk<'_> {
    /// Reads a function's body as a holder of its own, named for the function.
    fn function(&mut self, attrs: &[Attribute], name: &Ident, block: &Block) {
        if !prodlines::is_test_gated(attrs) {
            self.body = Some(name.to_string());
            self.visit_block(block);
        }
    }
}

impl<'ast> Visit<'ast> for Walk<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !prodlines::is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if !prodlines::is_test_gated(&node.attrs) {
            visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.function(&node.attrs, &node.sig.ident, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.function(&node.attrs, &node.sig.ident, &node.block);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        if let Some(block) = &node.default {
            self.function(&node.attrs, &node.sig.ident, block);
        }
    }

    fn visit_block(&mut self, node: &'ast Block) {
        let body = self.body.take();
        let parts = node
            .stmts
            .iter()
            .map(|stmt| {
                let mut lets = Facts::default();
                if let Stmt::Local(local) = stmt {
                    lets.visit_pat(&local.pat);
                }
                let mut facts = Facts::default();
                facts.visit_stmt(stmt);
                (stmt.span(), facts, lets.bound)
            })
            .collect();
        self.corpus.hold(body, parts);
        visit::visit_block(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        let parts = node
            .arms
            .iter()
            .map(|arm| {
                let mut facts = Facts::default();
                facts.visit_arm(arm);
                (arm.span(), facts, Vec::new())
            })
            .collect();
        self.corpus.hold(None, parts);
        visit::visit_expr_match(self, node);
    }
}

/// The names a statement or arm binds, and whether it leaves its function or a loop around it.
/// A labelled `break` or `continue` counts as leaving, whichever loop it names.
#[derive(Debug, Default)]
struct Facts {
    bound: Vec<String>,
    escapes: bool,
    loops: u32,
    closures: u32,
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_pat_ident(&mut self, node: &'ast PatIdent) {
        self.bound.push(node.ident.to_string());
        visit::visit_pat_ident(self, node);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        self.escapes |= match node {
            Expr::Return(_) => self.closures == 0,
            Expr::Break(ExprBreak { label, .. }) | Expr::Continue(ExprContinue { label, .. }) => {
                self.loops == 0 || label.is_some()
            }
            _ => false,
        };
        let looped = u32::from(matches!(
            node,
            Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_)
        ));
        let closed = u32::from(matches!(node, Expr::Closure(_) | Expr::Async(_)));
        self.loops += looped;
        self.closures += closed;
        visit::visit_expr(self, node);
        self.loops -= looped;
        self.closures -= closed;
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        self.escapes |= jumps(node.tokens.clone());
    }

    fn visit_item(&mut self, _: &'ast Item) {}
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// Three statements over six lines and 43 tokens; `value` is the one literal that differs.
    fn run(value: &str) -> String {
        format!(
            "    let total = items\n        .iter()\n        .map(|item| item.size * 2 + offset)\n        \
             .sum::<u64>();\n    let mean = total / count;\n    log(\"{value}\", total, mean);\n"
        )
    }

    /// Each body in a function of its own, closed by a macro no other function names, so a run ends
    /// there and no body is a run alone. The macro reads `tail`.
    fn spread(bodies: &[String], tail: &str) -> String {
        (bodies.iter().enumerate())
            .map(|(at, body)| format!("fn f{at}() {{\n{body}    m{at}!({tail});\n}}\n"))
            .collect()
    }

    fn found(src: &str) -> Vec<Repeat> {
        let mut corpus = Corpus::default();
        corpus.add("src/lib.rs", src);
        corpus.repeats()
    }

    /// The group three copies of `run` make in src/lib.rs, starting at `lines`, six lines each.
    fn function(saves: u32, lines: [u32; 3], last: u32) -> Repeat {
        Repeat {
            sites: lines
                .map(|line| site("src/lib.rs", line, line + last))
                .to_vec(),
            merge: Merge::Function,
            saves,
            differs: vec![r#"`"a"` / `"b"` / `"c"`"#.to_string()],
        }
    }

    fn site(file: &str, line: u32, last: u32) -> Site {
        Site {
            file: file.to_string(),
            line,
            last,
        }
    }

    fn copies(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| run(value)).collect()
    }

    #[test]
    fn three_copies_of_one_run_are_one_function_whatever_each_copy_names_for_itself() {
        let named = [function(6, [2, 11, 20], 5)];
        assert_eq!(found(&spread(&copies(&["a", "b", "c"]), "")), named);
        let own = [run("a"), run("b").replace("total", "sum"), run("c")];
        assert_eq!(found(&spread(&own, "")), named);
        // A function returns one value, so the code after a copy may read one name the copy binds.
        assert_eq!(found(&spread(&copies(&["a", "b", "c"]), "mean")), named);
    }

    #[test]
    fn copies_too_few_to_pay_for_a_function_or_binding_two_names_read_later_are_not_named() {
        for src in [
            spread(&copies(&["a", "b"]), ""),
            spread(&copies(&["a", "b", "c"]), "mean, total"),
            format!(
                "#[cfg(test)]\nmod tests {{\n{}}}\n",
                spread(&copies(&["a", "b", "c"]), "")
            ),
        ] {
            assert_eq!(found(&src), [], "{src}");
        }
    }

    #[test]
    fn copies_that_differ_in_a_string_a_macro_reads_merge_only_where_it_formats_the_same() {
        let logged = |value: &str| run(value).replace("log(", "log!(");
        assert_eq!(found(&spread(&["a", "b", "c"].map(logged), "")), []);
        let named = ["total", "sum", "all"]
            .map(|name| logged(&format!("{{{name}}}")).replace("total", name));
        let mut same = function(7, [2, 11, 20], 5);
        same.differs.clear();
        assert_eq!(found(&spread(&named, "")), [same]);
    }

    #[test]
    fn copies_that_call_another_method_are_not_one_shape() {
        let called = ["sum", "product", "count"].map(|method| run("a").replace("sum", method));
        assert_eq!(found(&spread(&called, "")), []);
        let fields = ["size", "len", "weight"].map(|field| run("a").replace("size", field));
        let mut read = function(6, [2, 11, 20], 5);
        read.differs = vec!["`size` / `len` / `weight`".to_string()];
        assert_eq!(found(&spread(&fields, "")), [read]);
    }

    /// A method and a trait's default body are bodies like a free function's; a match is read arm
    /// by arm, and a file that does not parse adds nothing.
    #[test]
    fn copies_in_methods_and_default_bodies_repeat_like_copies_in_free_functions() {
        let src = format!(
            "struct S;\nimpl S {{\n    fn f0() {{\n{a}    m0!();\n    }}\n}}\ntrait T {{\n    \
             fn f1() {{\n{b}    m1!((1));\n    }}\n}}\nfn f2() {{\n{c}    m2!();\n}}\n\
             fn g(x: u8) -> u8 {{\n    match x {{\n        0 => 1,\n        _ => 2,\n    }}\n}}\n",
            a = run("a"),
            b = run("b"),
            c = run("c"),
        );
        let mut corpus = Corpus::default();
        corpus.add("src/bad.rs", "fn broken( {\n");
        corpus.add("src/lib.rs", &src);
        assert_eq!(corpus.repeats(), [function(6, [4, 15, 25], 5)]);
    }

    #[test]
    fn copies_whose_own_names_stand_in_other_places_are_not_one_shape() {
        let paired = |first: &str, second: &str| {
            let sum = format!("|(item, other)| {first}.size * 2 + {second}.size");
            run("a").replace("|item| item.size * 2 + offset", &sum)
        };
        let swapped = [
            paired("item", "other"),
            paired("item", "other"),
            paired("other", "item"),
        ];
        assert_eq!(found(&spread(&swapped, "")), []);
        let mut renamed = swapped;
        renamed[2] = paired("item", "other").replace("|(item, other)| item.", "|(it, other)| it.");
        let mut same = function(7, [2, 11, 20], 5);
        same.differs.clear();
        assert_eq!(found(&spread(&renamed, "")), [same]);
    }

    #[test]
    fn a_format_string_reads_without_the_names_it_formats() {
        assert_eq!(unnamed(r#""{told}: {next}""#), r#""{}: {}""#);
        assert_eq!(unnamed(r#""{{kept}} {0:>5}""#), r#""{kept}} {:>5}""#);
    }

    #[test]
    fn a_return_ends_a_run_where_a_call_in_its_place_would_not() {
        let ending = |inner: &str| {
            let bodies = ["a", "b", "c"].map(|value| {
                format!(
                    "{}    if mean == 0 {{\n        {inner};\n    }}\n",
                    run(value)
                )
            });
            found(&spread(&bodies, ""))
        };
        assert_eq!(ending("stop()"), [function(12, [2, 14, 26], 8)]);
        assert_eq!(ending("return"), [function(6, [2, 14, 26], 5)]);
    }

    #[test]
    fn rows_side_by_side_are_one_table_while_four_values_differ() {
        let rows = |fifth: &dyn Fn(u32) -> String| {
            let rows: String = (1..=4)
                .map(|at| {
                    format!(
                        "    report(\n        a{at},\n        b{at},\n        c{at},\n        d{at},\n        \
                         {},\n        &settings.layout.margins,\n        &settings.layout.columns,\n        \
                         settings.layout.padding,\n    );\n",
                        fifth(at)
                    )
                })
                .collect();
            found(&format!("fn rows() {{\n{rows}}}\n"))
        };
        let differs =
            ["a", "b", "c", "d"].map(|it| format!("`{it}1` / `{it}2` / `{it}3` / `{it}4`"));
        assert_eq!(
            rows(&|_| "shared".to_string()),
            [Repeat {
                sites: [2, 12, 22, 32]
                    .map(|line| site("src/lib.rs", line, line + 9))
                    .to_vec(),
                merge: Merge::Table,
                saves: 24,
                differs: differs.to_vec(),
            }]
        );
        assert_eq!(rows(&|at| format!("e{at}")), []);
    }

    #[test]
    fn a_function_whose_whole_body_is_a_copy_is_called_in_place_of_the_others() {
        let shared = format!("fn shared() {{\n{}}}\n", run("a"));
        let named = Repeat {
            sites: [2, 10, 19]
                .map(|line| site("src/lib.rs", line, line + 5))
                .to_vec(),
            merge: Merge::Call("shared".to_string()),
            saves: 10,
            differs: function(0, [0; 3], 0).differs,
        };
        assert_eq!(
            found(&(shared + &spread(&copies(&["b", "c"]), ""))),
            [named]
        );
        let bodies: String = (["a", "b", "c"].iter().enumerate())
            .map(|(at, value)| format!("fn s{at}() {{\n{}}}\n", run(value)))
            .collect();
        assert_eq!(found(&bodies), []);
    }

    /// A copy spans three lines or more and thirty tokens or more.
    #[test]
    fn a_copy_shorter_than_three_lines_or_thirty_tokens_is_not_named() {
        let names = "x1, x2, x3, x4, x5, x6,\n        x7, x8, x9, x10, x11, x12";
        let saves = |copies: usize, call: &dyn Fn(&str) -> String| {
            let bodies: Vec<String> = (0..copies).map(|at| call(&format!("v{at}"))).collect();
            found(&spread(&bodies, ""))
                .iter()
                .map(|it| it.saves)
                .collect::<Vec<u32>>()
        };
        let two_lines = |value: &str| format!("    call(\"{value}\", {names}, x13);\n");
        let three_lines = |value: &str| format!("    call(\n        \"{value}\", {names}, x13);\n");
        let fewer_tokens = |value: &str| format!("    call(\n        \"{value}\", {names},);\n");
        assert_eq!(saves(10, &two_lines), Vec::<u32>::new());
        assert_eq!(saves(10, &three_lines), [14]);
        assert_eq!(saves(6, &three_lines), [6]);
        assert_eq!(saves(6, &fewer_tokens), Vec::<u32>::new());
    }

    #[test]
    fn a_group_merges_as_a_table_where_its_copies_stand_side_by_side() {
        let mut corpus = Corpus::default();
        corpus.add("src/lib.rs", &spread(&copies(&["a", "b", "c"]), ""));
        let judged = |tandem| {
            corpus
                .judge(&[0, 4, 8], 3, tandem)
                .map(|it| (it.merge, it.saves))
        };
        assert_eq!(judged(false), Some((Merge::Function, 6)));
        assert_eq!(judged(true), Some((Merge::Table, 7)));
    }

    #[test]
    fn a_corpus_knows_whole_bodies_names_read_after_a_run_and_values_that_differ() {
        let mut corpus = Corpus::default();
        corpus.add(
            "src/lib.rs",
            "fn whole() {\n    let a = 1;\n    let b = a;\n}\nfn later() {\n    let a = 1;\n    let b = 2;\n    \
             use_it(a);\n    let b = 3;\n    use_it(b);\n}\n",
        );
        corpus.add("src/t.rs", "#![cfg(test)]\nfn t() {\n    let a = 1;\n}\n");
        assert_eq!(corpus.files, ["src/lib.rs"]);
        let (symbols, units) = corpus.symbols();
        assert_eq!(
            units,
            [
                Some(0),
                Some(1),
                None,
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                None
            ]
        );
        assert!(symbols[2] > 1 << 63 && symbols[8] > symbols[2]);
        assert_eq!(
            symbols[0], symbols[3],
            "`let a = 1;` is one shape wherever it stands"
        );
        assert_eq!(corpus.whole(0, 2), Some("whole"));
        assert_eq!((corpus.whole(0, 1), corpus.whole(2, 2)), (None, None));
        // `a` is read after the run; `b` is only bound again.
        assert_eq!(corpus.outliving(2, 2), 1);
        assert_eq!(corpus.outliving(0, 1), 1);
        let differs = corpus
            .differs(&[&corpus.units[0..2], &corpus.units[2..4]])
            .unwrap();
        let shown: Vec<String> = differs.iter().map(|tuple| corpus.shown(tuple)).collect();
        assert_eq!(shown, ["`a` / `2`"]);
        let repeated = corpus.ids(["x", "x", "y"].map(String::from).to_vec());
        assert_eq!(corpus.shown(&repeated), "`x` / `y`");
        let many = corpus.ids((1..=6).map(|it| it.to_string()).collect());
        assert_eq!(corpus.shown(&many), "`1` / `2` / `3` / `4` and 2 more");
        let long = corpus.ids(vec!["a".repeat(30)]);
        assert_eq!(corpus.shown(&long), format!("`{}…`", "a".repeat(24)));
    }

    #[test]
    fn the_suffixes_of_a_sequence_sort_share_prefixes_and_group_into_runs() {
        let symbols = [1, 2, 1, 2, 1, 3, 1, 2];
        let order = suffixes(&symbols);
        assert_eq!(order, [6, 0, 2, 4, 7, 1, 3, 5]);
        let shared = common(&symbols, &order);
        assert_eq!(shared, [0, 2, 3, 1, 0, 1, 2, 0]);
        let mut runs = intervals(&order, &shared);
        runs.iter_mut()
            .for_each(|(_, starts)| starts.sort_unstable());
        runs.sort();
        let expected: [(usize, &[usize]); 5] = [
            (1, &[0, 2, 4, 6]),
            (1, &[1, 3, 7]),
            (2, &[0, 2, 6]),
            (2, &[1, 3]),
            (3, &[0, 2]),
        ];
        assert_eq!(
            runs,
            expected.map(|(length, starts)| (length, starts.to_vec()))
        );
        assert_eq!((suffixes(&[]), common(&[], &[])), (Vec::new(), Vec::new()));
    }

    #[test]
    fn the_group_removing_most_lines_wins_a_line_two_groups_share() {
        let group = |saves, sites: Vec<Site>| Repeat {
            sites,
            merge: Merge::Function,
            saves,
            differs: Vec::new(),
        };
        let most = group(10, vec![site("a.rs", 1, 5), site("a.rs", 10, 15)]);
        let edge = group(8, vec![site("a.rs", 5, 8), site("a.rs", 20, 25)]);
        let apart = group(7, vec![site("a.rs", 16, 19), site("b.rs", 1, 5)]);
        let kept = settle(vec![apart.clone(), edge, most.clone()]);
        assert_eq!(kept, [most, apart]);
    }

    #[test]
    fn each_copy_carries_a_share_of_the_lines_and_the_fix_names_the_merge() {
        let mut repeat = Repeat {
            sites: vec![site("a.rs", 1, 4), site("a.rs", 9, 12), site("b.rs", 3, 6)],
            merge: Merge::Function,
            saves: 8,
            differs: vec!["`x` / `y` / `z`".to_string()],
        };
        assert_eq!(repeat.shares(), [4, 2, 2]);
        assert_eq!(
            repeat.places()[2],
            Place::at("copy 3 of 3", "b.rs", 3).through(6)
        );
        assert_eq!(
            repeat.fix(),
            "move the 3 copies into one function with a parameter for `x` / `y` / `z`, and call it \
             from each copy; that removes 8 lines"
        );
        repeat.differs.push("`p` / `q`".to_string());
        assert!(
            repeat
                .fix()
                .contains(" with parameters for `x` / `y` / `z`, `p` / `q`, and ")
        );
        repeat.differs.clear();
        assert_eq!(
            repeat.fix(),
            "move the 3 copies into one function, and call it from each copy; that removes 8 lines"
        );
        repeat.merge = Merge::Table;
        assert_eq!(
            repeat.places()[0],
            Place::at("row 1 of 3", "a.rs", 1).through(4)
        );
        assert_eq!(
            repeat.fix(),
            "write the 3 rows once, as a loop or one arm over a table; that removes 8 lines"
        );
        repeat.merge = Merge::Call("shared".to_string());
        assert_eq!(
            repeat.fix(),
            "call `shared` in place of each other copy; that removes 8 lines"
        );
    }

    #[test]
    fn a_shape_keeps_keywords_and_macro_names_and_reads_every_other_name_as_a_value() {
        let shaped = |src: &str| {
            let mut hasher = DefaultHasher::new();
            let mut leaves = Vec::new();
            let count = shape(src.parse().unwrap(), false, &mut hasher, &mut leaves);
            (hasher.finish(), leaves, count)
        };
        let (one, leaves, count) = shaped("let x = f(1);");
        assert_eq!(
            (leaves, count),
            (vec!["x".to_string(), "f".into(), "1".into()], 7)
        );
        assert_eq!(shaped("let y = g(25);").0, one);
        for other in ["let y = g(\"2\");", "let y = g!(2);", "if y = g(2);"] {
            assert_ne!(shaped(other).0, one, "{other}");
        }
        assert_ne!(shaped("let y = g!(2);").0, shaped("let y = h!(2);").0);
        let marked = |text: &str| format!("{FORMAT}{text}");
        let (_, leaves, _) = shaped(r#"log!("{told}", g("a"), x); g("b");"#);
        let expected = [marked(r#""{}""#), "g".into(), marked(r#""a""#), "x".into()];
        assert_eq!(
            leaves,
            [&expected[..], &["g".into(), r#""b""#.into()]].concat()
        );
    }

    #[test]
    fn a_statement_names_what_it_binds_and_whether_it_leaves_its_function_or_loop() {
        for (src, escapes, bound) in [
            ("let total = loop { break 1; };", false, "total"),
            ("let f = |x| return x;", false, "f x"),
            ("for item in items { continue; }", false, "item"),
            (
                "if let Some(found) = held { use_it(found) }",
                false,
                "found",
            ),
            ("fn inner() { return; }", false, ""),
            ("return;", true, ""),
            ("break;", true, ""),
            ("'outer: for a in b { break 'outer; }", true, "a"),
            ("check!(return);", true, ""),
        ] {
            let mut facts = Facts::default();
            facts.visit_stmt(&syn::parse_str::<Stmt>(src).unwrap());
            assert_eq!(
                (facts.escapes, facts.bound.join(" ").as_str()),
                (escapes, bound),
                "{src}"
            );
        }
    }
}
