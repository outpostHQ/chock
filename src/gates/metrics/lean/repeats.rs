//! Runs of statements or match arms that repeat in one shape, in production code or in tests, and
//! the lines that one function, closure, table or test over a table, or one call, would remove.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Block, Expr, ExprBreak, ExprContinue, ExprMatch, Ident, ImplItemFn, Item, ItemFn,
    ItemImpl, ItemMod, Macro, PatIdent, Stmt, TraitItemFn,
};

use crate::gates::metrics::prodlines;
use crate::run::report::Place;
use shape::{Inside, shape};

mod shape;

/// The fewest lines a copy spans for its run to be worth a name of its own.
const LINES: u32 = 3;
/// The fewest tokens in a copy, so a few lines of punctuation never count.
const TOKENS: usize = 30;
/// The most names or values that may differ between copies one function merges: a parameter each.
const VALUES: usize = 6;
/// The most values that may differ between rows or cases one table merges: a column each.
const COLUMNS: usize = 12;
/// The widest call or table row rustfmt keeps on one line: its 100 columns less a table's indent.
const WIDTH: usize = 88;
/// The most names a copy binds that the code after it reads: the merged function returns them.
const OUTLIVE: usize = 3;
/// The fewest lines a merge must remove to be named. A judgment: below it, the jump a reader makes
/// to the shared code costs more than the lines it saves.
const SAVES: i64 = 6;
/// The most distinct values a fix names for one parameter.
const SHOWN: usize = 4;
/// Marks a string a macro reads that does not format it. A function cannot take one as a
/// parameter, so copies that differ in one do not merge.
const FORMAT: char = '\u{1}';
/// Marks a string a format macro reads. Copies that differ in one merge where each has the same
/// placeholders, since the function can pass the text that differs to a placeholder of its own.
const TEXT: char = '\u{2}';
/// How a group of copies merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// Into one new function that each copy calls.
    Function,
    /// Side by side in one block or match: into one table of rows, and the shared lines once.
    Table,
    /// Into a call to the function whose whole body is one of the copies.
    Call(String),
    /// Into one function that takes the one statement that differs between copies as a closure.
    Closure,
    /// Tests whose whole bodies repeat: into one test that loops over a table of cases.
    Cases,
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

    /// Each file's part of the lines the merge removes.
    pub fn by_file(&self) -> BTreeMap<&str, u64> {
        let mut lines = BTreeMap::new();
        for (site, share) in self.sites.iter().zip(self.shares()) {
            *lines.entry(site.file.as_str()).or_default() += share;
        }
        lines
    }

    /// Each copy as a place, named as a copy or a row of the group.
    pub fn places(&self) -> Vec<Place> {
        let role = match self.merge {
            Merge::Table => "row",
            Merge::Cases => "case",
            Merge::Function | Merge::Call(_) | Merge::Closure => "copy",
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
            Merge::Closure => format!(
                "move the {copies} copies into one function{parameter}{values} that takes the \
                 statement that differs as a closure, and call it from each copy; that removes \
                 {saves} lines"
            ),
            Merge::Cases => format!(
                "make the {copies} tests one test that loops over a table{values}; that removes \
                 {saves} lines"
            ),
        }
    }
}

/// A block or a match that holds units: its file, its first unit, how many it holds, the function's
/// name where it is a whole function body, and whether that function is a test.
#[derive(Debug)]
struct Holder {
    file: usize,
    first: usize,
    len: usize,
    body: Option<String>,
    case: bool,
    test: bool,
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

/// The statements and arms of every file read so far, each name kept once.
#[derive(Debug, Default)]
pub struct Corpus {
    files: Vec<String>,
    holders: Vec<Holder>,
    units: Vec<Unit>,
    names: HashMap<String, u32>,
    texts: Vec<String>,
}

impl Corpus {
    /// Reads one file's blocks and matches, all of them test code where `test` says so or the file
    /// builds only for tests. A file that does not parse adds nothing.
    pub fn add(&mut self, shown: &str, src: &str, test: bool) {
        let Ok(file) = prodlines::parse_rust(src) else {
            return;
        };
        self.files.push(shown.to_string());
        Walk {
            corpus: self,
            body: None,
            test: test || prodlines::is_test_gated(&file.attrs),
        }
        .visit_file(&file);
    }

    /// The file and first line of each statement and arm of test code. A group is all test code
    /// or all production code, so a group's first copy says which the group is.
    pub fn test_starts(&self) -> BTreeSet<(&str, u32)> {
        (self.units.iter())
            .filter(|unit| self.holders[unit.holder].test)
            .map(|unit| {
                (
                    self.files[self.holders[unit.holder].file].as_str(),
                    unit.line,
                )
            })
            .collect()
    }

    /// The groups to name, most lines removed first; no two share a line.
    pub fn repeats(&self) -> Vec<Repeat> {
        let (symbols, units) = self.symbols();
        let order = suffixes(&symbols);
        let shared = common(&symbols, &order);
        let mut found = Vec::new();
        for (length, starts) in intervals(&order, &shared) {
            let before: BTreeSet<Option<u64>> = starts
                .iter()
                .map(|at| at.checked_sub(1).map(|it| symbols[it]))
                .collect();
            // One symbol before every copy: the run is part of a longer one.
            if before.len() == 1 {
                continue;
            }
            let apart = apart(starts, length);
            let tandem = apart.windows(2).all(|pair| pair[1] == pair[0] + length);
            let firsts: Vec<usize> = apart.iter().filter_map(|&at| units[at]).collect();
            found.extend(self.judge(&firsts, length, None, tandem));
            found.extend(self.gapped(&symbols, &units, &apart, length));
        }
        settle(found)
    }

    /// The groups among copies of a run of `length` that go on alike after the one unit that
    /// differs: the run, that unit, and every unit after it that the copies share.
    fn gapped(
        &self,
        symbols: &[u64],
        units: &[Option<usize>],
        starts: &[usize],
        length: usize,
    ) -> Vec<Repeat> {
        let mut resumed: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        // Each holder ends in a symbol of its own, so a copy that ends at its gap resumes alone.
        for &at in starts {
            if units[at + length].is_some() {
                resumed
                    .entry(symbols[at + length + 1])
                    .or_default()
                    .push(at);
            }
        }
        let mut found = Vec::new();
        for starts in resumed.into_values().filter(|starts| starts.len() > 1) {
            let alike = |more: usize| {
                (starts.iter()).all(|&at| symbols.get(at + more) == symbols.get(starts[0] + more))
            };
            let mut span = length + 1;
            while alike(span) {
                span += 1;
            }
            let apart = apart(starts, span);
            let firsts: Vec<usize> = apart.iter().filter_map(|&at| units[at]).collect();
            found.extend(self.judge(&firsts, span, Some(length), false));
        }
        found
    }

    /// One symbol per unit, holder after holder, then one of its own after each holder, so that no
    /// run crosses into the next. A unit that may not move is a symbol of its own too.
    fn symbols(&self) -> (Vec<u64>, Vec<Option<usize>>) {
        let mut symbols = Vec::new();
        let mut units = Vec::new();
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

    /// The group whose copies start at `firsts` and run `length` units, the one at `gap` left to
    /// each copy, if merging it removes enough lines and nothing stands in the way.
    fn judge(
        &self,
        firsts: &[usize],
        length: usize,
        gap: Option<usize>,
        tandem: bool,
    ) -> Option<Repeat> {
        let runs: Vec<&[Unit]> = firsts
            .iter()
            .map(|&first| &self.units[first..first + length])
            .collect();
        let lines: Vec<u32> = runs
            .iter()
            .map(|run| {
                let kept = gap.map_or(0, |at| run[at].last + 1 - run[at].line);
                (run[length - 1].last + 1).saturating_sub(run[0].line + kept)
            })
            .collect();
        let tokens: usize = shared(runs.first()?, gap).map(|unit| unit.tokens).sum();
        let least = *lines.iter().min()?;
        let held: Vec<Option<&Holder>> = firsts
            .iter()
            .map(|&first| self.holding(first, length))
            .collect();
        let cases = held.iter().all(|it| it.is_some_and(|holder| holder.case));
        let whole: Vec<Option<&str>> = held
            .iter()
            .map(|it| it.filter(|holder| !holder.case)?.body.as_deref())
            .collect();
        let outlive = firsts
            .iter()
            .map(|&first| self.outliving(first, length))
            .max()?;
        let escapes = gap.is_some_and(|at| runs.iter().any(|run| run[at].symbol.is_none()));
        if runs.len() < 2 || tokens < TOKENS || least < LINES || outlive > OUTLIVE || escapes {
            return None;
        }
        let differs = self.differs(&runs, gap)?;
        let removed: i64 = lines.iter().map(|&it| i64::from(it) - 1).sum();
        let bodies: i64 = (lines.iter().zip(&whole))
            .filter_map(|(&it, whole)| whole.map(|_| i64::from(it) - 1))
            .sum();
        let rows = i64::try_from(runs.len()).ok()?;
        let values = i64::try_from(differs.len()).ok()?;
        let least = i64::from(least);
        let kept: i64 = (0..runs.len())
            .map(|at| row(&self.widths(&differs, at)))
            .sum();
        // Each copy leaves its values behind, in a call or a row, on the lines rustfmt gives them.
        let removed = removed + rows - kept;
        let (merge, saves, width) = match whole.iter().flatten().next() {
            _ if whole.iter().all(Option::is_some) => return None,
            _ if tandem => (Merge::Table, removed - (least + 2), COLUMNS),
            _ if gap.is_some() => (Merge::Closure, removed - (least + 4 + values), VALUES),
            _ if cases => (Merge::Cases, removed - (least + 2), COLUMNS),
            Some(name) => (Merge::Call((*name).to_string()), removed - bodies, VALUES),
            None => (Merge::Function, removed - (least + 2 + values), VALUES),
        };
        if saves < SAVES || differs.len() > width {
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

    /// The holder whose whole body is the run of `length` units from `first`, where one is.
    fn holding(&self, first: usize, length: usize) -> Option<&Holder> {
        let holder = &self.holders[self.units[first].holder];
        (holder.first == first && holder.len == length).then_some(holder)
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

    /// Each distinct tuple of values that differs across the copies outside the unit at `gap`,
    /// leaving out names each copy binds; none where one cannot be passed or bound names move.
    fn differs(&self, runs: &[&[Unit]], gap: Option<usize>) -> Option<Vec<Vec<u32>>> {
        let leaves: Vec<Vec<u32>> = runs
            .iter()
            .map(|run| {
                shared(run, gap)
                    .flat_map(|unit| unit.leaves.clone())
                    .collect()
            })
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
            if !same && !self.passed(&tuple) {
                return None;
            }
            if !same && !own && !out.contains(&tuple) {
                out.push(tuple);
            }
        }
        Some(out)
    }

    /// Whether a function can take each of a tuple's values: never a string a macro reads unless
    /// that macro formats it, and then only where each copy has the same placeholders.
    fn passed(&self, tuple: &[u32]) -> bool {
        let texts: Vec<&str> = (tuple.iter())
            .map(|&id| self.texts[id as usize].as_str())
            .collect();
        let first = texts
            .first()
            .and_then(|it| it.strip_prefix(TEXT))
            .map(placeholders);
        (texts.iter())
            .all(|it| !it.starts_with(FORMAT) && it.strip_prefix(TEXT).map(placeholders) == first)
    }

    /// How wide each value that differs is in the copy at `at`, as its source spells it.
    fn widths(&self, differs: &[Vec<u32>], at: usize) -> Vec<usize> {
        (differs.iter())
            .map(|tuple| {
                self.texts[tuple[at] as usize]
                    .trim_start_matches([TEXT, FORMAT])
                    .len()
            })
            .collect()
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
                let text = self.texts[id as usize].trim_start_matches(TEXT);
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
    /// names a `let` binds. Production code and the tests of each directory are shapes apart.
    fn hold(&mut self, body: Option<(String, bool)>, test: bool, parts: Vec<Part>) {
        let file = self.files.len() - 1;
        let side = test.then(|| reach(&self.files[file]));
        let holder = self.holders.len();
        self.holders.push(Holder {
            file,
            first: self.units.len(),
            len: parts.len(),
            case: body.as_ref().is_some_and(|(_, case)| *case),
            body: body.map(|(name, _)| name),
            test,
        });
        for (span, facts, lets) in parts {
            let mut hasher = DefaultHasher::new();
            side.hash(&mut hasher);
            let mut leaves = Vec::new();
            let read = span.source_text().and_then(|text| text.parse().ok());
            let tokens = read.map(|tokens| shape(tokens, Inside::Code, &mut hasher, &mut leaves));
            let unit = Unit {
                holder,
                line: u32::try_from(span.start().line).unwrap_or(u32::MAX),
                last: u32::try_from(span.end().line).unwrap_or(u32::MAX),
                symbol: tokens
                    .filter(|_| !facts.escapes)
                    .map(|_| hasher.finish() >> 1),
                tokens: tokens.unwrap_or(0),
                leaves: self.ids(leaves),
                bound: self.ids(facts.bound),
                lets: self.ids(lets),
            };
            self.units.push(unit);
        }
    }
}

/// A statement or arm as the walk hands it over: its span, its facts and the names a `let` binds.
type Part = (Span, Facts, Vec<String>);

/// The starts in order, each `length` or more units after the one kept before it.
fn apart(mut starts: Vec<usize>, length: usize) -> Vec<usize> {
    starts.sort_unstable();
    let mut apart: Vec<usize> = Vec::new();
    for at in starts {
        if apart.last().is_none_or(|last| at >= last + length) {
            apart.push(at);
        }
    }
    apart
}

/// The lines rustfmt gives values of these widths in a call or a table row: one while they fit
/// in `WIDTH` columns, else one each and one on either side. A lone value never breaks.
fn row(widths: &[usize]) -> i64 {
    let width = widths.iter().map(|it| it + 2).sum::<usize>() + 1;
    match i64::try_from(widths.len()) {
        Ok(values @ 2..) if width > WIDTH => values + 2,
        _ => 1,
    }
}

/// The units of a copy that its group shares: all but the one at `gap`.
fn shared(run: &[Unit], gap: Option<usize>) -> impl Iterator<Item = &Unit> {
    (run.iter().enumerate())
        .filter(move |&(at, _)| Some(at) != gap)
        .map(|(_, unit)| unit)
}

/// The directory whose code the tests in a file can call: a crate's `src`, or its `tests`
/// directory, which builds apart from `src`.
fn reach(shown: &str) -> String {
    let dirs = shown.rsplit_once('/').map_or("", |(dirs, _)| dirs);
    let parts: Vec<&str> = dirs.split('/').collect();
    let end = parts.iter().position(|it| *it == "src" || *it == "tests");
    parts[..end.map_or(0, |at| at + 1)].join("/")
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

/// Whether each copy's own name at one place pairs with the first copy's as at every place before,
/// both ways, as a rename would leave them. `pairs` holds each copy's pairs.
fn renamed(tuple: &[u32], pairs: &mut [HashMap<(bool, u32), u32>]) -> bool {
    tuple.iter().zip(pairs).all(|(&name, pairs)| {
        *pairs.entry((true, tuple[0])).or_insert(name) == name
            && *pairs.entry((false, name)).or_insert(tuple[0]) == tuple[0]
    })
}

/// The placeholders of a format string whose names are out, in order; `{{` is text, not one.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open..];
        if let Some(after) = rest.strip_prefix("{{") {
            rest = after;
        } else {
            let close = rest.find('}').map_or(rest.len(), |it| it + 1);
            found.push(&rest[..close]);
            rest = &rest[close..];
        }
    }
    found
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
        if usize::try_from(distinct).is_ok_and(|it| it == symbols.len()) {
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

/// Walks one file's items and hands each block's statements and each match's arms to the corpus,
/// as test code inside an item that builds only for tests or a function a test attribute marks.
struct Walk<'a> {
    corpus: &'a mut Corpus,
    body: Option<(String, bool)>,
    test: bool,
}

impl Walk<'_> {
    /// Visits an item, as test code where its attributes build it only for tests.
    fn within(&mut self, attrs: &[Attribute], visit: impl FnOnce(&mut Self)) {
        let test = self.test;
        self.test |= prodlines::is_test_gated(attrs);
        visit(self);
        self.test = test;
    }

    /// Reads a function's body as a holder of its own, named for the function: a test case where
    /// an attribute such as `#[test]` or `#[tokio::test]` marks it.
    fn function(&mut self, attrs: &[Attribute], name: &Ident, block: &Block) {
        let case = (attrs.iter())
            .any(|attr| (attr.path().segments.last()).is_some_and(|it| it.ident == "test"));
        self.within(attrs, |walk| {
            walk.test |= case;
            walk.body = Some((name.to_string(), case));
            walk.visit_block(block);
        });
    }
}

impl<'ast> Visit<'ast> for Walk<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        self.within(&node.attrs, |walk| visit::visit_item_mod(walk, node));
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        self.within(&node.attrs, |walk| visit::visit_item_impl(walk, node));
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
        self.corpus.hold(body, self.test, parts);
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
        self.corpus.hold(None, self.test, parts);
        visit::visit_expr_match(self, node);
    }
}

/// The names a statement or arm binds, and whether it leaves its function or a loop around it.
/// A labelled `break` or `continue` counts as leaving, whichever loop it names.
#[derive(Debug, Default)]
pub(super) struct Facts {
    pub(super) bound: Vec<String>,
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
    use super::shape::unnamed;
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

    /// One file's corpus. Each test calls `repeats` itself, so mutest's call depth reaches `judge`.
    fn lib(src: &str) -> Corpus {
        let mut corpus = Corpus::default();
        corpus.add("src/lib.rs", src, false);
        corpus
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

    /// A six-line `report` call; its second line holds one value per column, each named for `at`.
    fn report(columns: usize, at: usize) -> String {
        let values: String = (0..columns).map(|c| format!(" v{c}_{at},")).collect();
        format!(
            "    report(\n       {values}\n        &settings.layout.margins,\n        \
             &settings.layout.columns,\n        settings.layout.padding,\n    );\n"
        )
    }

    #[test]
    fn three_copies_of_one_run_are_one_function_whatever_each_copy_names_for_itself() {
        let named = [function(6, [2, 11, 20], 5)];
        assert_eq!(lib(&spread(&copies(&["a", "b", "c"]), "")).repeats(), named);
        let own = [run("a"), run("b").replace("total", "sum"), run("c")];
        assert_eq!(lib(&spread(&own, "")).repeats(), named);
        // A function returns one value, so the code after a copy may read one name the copy binds.
        assert_eq!(
            lib(&spread(&copies(&["a", "b", "c"]), "mean")).repeats(),
            named
        );
        let led = format!(
            "fn lead() {{\n    go();\n}}\n{}",
            spread(&copies(&["a", "b", "c"]), "")
        );
        assert_eq!(lib(&led).repeats(), [function(6, [5, 14, 23], 5)]);
    }

    #[test]
    fn copies_too_few_to_pay_for_a_function_or_binding_four_names_read_later_are_not_named() {
        assert_eq!(lib(&spread(&copies(&["a", "b"]), "")).repeats(), []);
        let bodies = ["a", "b", "c"]
            .map(|value| run(value) + "    let (low, high) = (total - 1, mean + 1);\n");
        assert_eq!(
            lib(&spread(&bodies, "low, high, mean")).repeats(),
            [function(8, [2, 12, 22], 6)]
        );
        assert_eq!(
            lib(&spread(&bodies, "low, high, mean, total")).repeats(),
            []
        );
    }

    #[test]
    fn copies_that_differ_in_a_string_a_macro_reads_merge_only_where_it_formats_the_same() {
        let logged = |value: &str| run(value).replace("log(", "log!(");
        assert_eq!(lib(&spread(&["a", "b", "c"].map(logged), "")).repeats(), []);
        let named = ["total", "sum", "all"]
            .map(|name| logged(&format!("{{{name}}}")).replace("total", name));
        let mut same = function(7, [2, 11, 20], 5);
        same.differs.clear();
        assert_eq!(lib(&spread(&named, "")).repeats(), [same]);
    }

    #[test]
    fn copies_that_call_another_method_are_not_one_shape() {
        let called = ["sum", "product", "count"].map(|method| run("a").replace("sum", method));
        assert_eq!(lib(&spread(&called, "")).repeats(), []);
        let fields = ["size", "len", "weight"].map(|field| run("a").replace("size", field));
        let mut read = function(6, [2, 11, 20], 5);
        read.differs = vec!["`size` / `len` / `weight`".to_string()];
        assert_eq!(lib(&spread(&fields, "")).repeats(), [read]);
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
        corpus.add("src/bad.rs", "fn broken( {\n", false);
        corpus.add("src/lib.rs", &src, false);
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
        assert_eq!(lib(&spread(&swapped, "")).repeats(), []);
        let mut renamed = swapped;
        renamed[2] = paired("item", "other").replace("|(item, other)| item.", "|(it, other)| it.");
        let mut same = function(7, [2, 11, 20], 5);
        same.differs.clear();
        assert_eq!(lib(&spread(&renamed, "")).repeats(), [same]);
    }

    #[test]
    fn a_format_string_reads_without_the_names_it_formats() {
        assert_eq!(unnamed(r#""{told}: {next}""#), r#""{}: {}""#);
        assert_eq!(unnamed(r#""{{kept}} {0:>5}""#), r#""{{kept}} {:>5}""#);
        assert_eq!(placeholders(r#""{{kept}} {:>5} {}""#), ["{:>5}", "{}"]);
        assert_eq!(placeholders(r#""{{}} {""#), [r#"{""#]);
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
            lib(&spread(&bodies, ""))
        };
        assert_eq!(ending("stop()").repeats(), [function(12, [2, 14, 26], 8)]);
        assert_eq!(ending("return").repeats(), [function(6, [2, 14, 26], 5)]);
    }

    #[test]
    fn values_too_wide_for_one_line_take_a_line_each_and_one_on_either_side() {
        assert_eq!(row(&[]), 1);
        assert_eq!(row(&[200]), 1, "a lone value never breaks");
        assert_eq!(row(&[42, 41]), 1, "`(a, b),` spans 88 columns");
        assert_eq!(row(&[42, 42]), 4);
        assert_eq!(row(&[10, 10, 70]), 5);
        let long = |at: usize| format!("{}{at}", "x".repeat(44));
        let rows = |name: &dyn Fn(usize) -> String| {
            let rows: String = (1..=4)
                .map(|at| report(11, at).replace(&format!("v0_{at}"), &name(at)))
                .collect();
            lib(&format!("fn rows() {{\n{rows}}}\n")).repeats()
        };
        assert_eq!(rows(&|at| format!("v0_{at}"))[0].saves, 12);
        let wide = rows(&|at| format!("{}, {}", long(at), long(at + 4)));
        assert_eq!(
            wide,
            [],
            "rows of two wide values keep four lines each and save nothing"
        );
    }

    #[test]
    fn rows_side_by_side_are_one_table_while_twelve_values_differ() {
        let rows = |columns| {
            let rows: String = (1..=4).map(|at| report(columns, at)).collect();
            lib(&format!("fn rows() {{\n{rows}}}\n"))
        };
        let differs = (0..12).map(|c| format!("`v{c}_1` / `v{c}_2` / `v{c}_3` / `v{c}_4`"));
        assert_eq!(
            rows(12).repeats(),
            [Repeat {
                sites: [2, 8, 14, 20]
                    .map(|line| site("src/lib.rs", line, line + 5))
                    .to_vec(),
                merge: Merge::Table,
                saves: 12,
                differs: differs.collect(),
            }]
        );
        assert_eq!(rows(13).repeats(), []);
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
            lib(&(shared + &spread(&copies(&["b", "c"]), ""))).repeats(),
            [named]
        );
        let bodies: String = (["a", "b", "c"].iter().enumerate())
            .map(|(at, value)| format!("fn s{at}() {{\n{}}}\n", run(value)))
            .collect();
        assert_eq!(lib(&bodies).repeats(), []);
        let led: String = (["a", "b", "c"].iter().enumerate())
            .map(|(at, value)| format!("fn s{at}() {{\n    let x = setup();\n{}}}\n", run(value)))
            .collect();
        assert_eq!(
            lib(&led).repeats(),
            [],
            "a run inside whole copies is judged with them"
        );
    }

    /// A copy spans three lines or more and thirty tokens or more. Two tall copies pay.
    #[test]
    fn a_copy_shorter_than_three_lines_or_thirty_tokens_is_not_named() {
        let names = "x1, x2, x3, x4, x5, x6,\n        x7, x8, x9, x10, x11, x12";
        let tree = |copies: usize, call: &dyn Fn(&str) -> String| {
            let bodies: Vec<String> = (0..copies).map(|at| call(&format!("v{at}"))).collect();
            lib(&spread(&bodies, ""))
        };
        let saves = |groups: &[Repeat]| groups.iter().map(|it| it.saves).collect::<Vec<u32>>();
        let two_lines = |value: &str| format!("    call(\"{value}\", {names}, x13);\n");
        let three_lines = |value: &str| format!("    call(\n        \"{value}\", {names}, x13);\n");
        let fewer_tokens = |value: &str| format!("    call(\n        \"{value}\", {names},);\n");
        let column: String = (1..=13).map(|at| format!("        x{at},\n")).collect();
        let tall = |value: &str| format!("    call(\n        \"{value}\",\n{column}    );\n");
        assert_eq!(saves(&tree(20, &two_lines).repeats()), Vec::<u32>::new());
        assert_eq!(saves(&tree(10, &three_lines).repeats()), [14]);
        assert_eq!(saves(&tree(6, &three_lines).repeats()), [6]);
        assert_eq!(saves(&tree(6, &fewer_tokens).repeats()), Vec::<u32>::new());
        assert_eq!(saves(&tree(2, &tall).repeats()), [11]);
    }

    #[test]
    fn a_group_merges_as_a_table_where_its_copies_stand_side_by_side() {
        let mut corpus = Corpus::default();
        corpus.add("src/lib.rs", &spread(&copies(&["a", "b", "c"]), ""), false);
        let judged = |tandem| {
            corpus
                .judge(&[0, 4, 8], 3, None, tandem)
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
            false,
        );
        corpus.add(
            "src/t.rs",
            "#![cfg(test)]\nfn t() {\n    let a = 1;\n}\n",
            false,
        );
        assert_eq!(corpus.files, ["src/lib.rs", "src/t.rs"]);
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
                None,
                Some(7),
                None
            ]
        );
        assert!(symbols[2] > 1 << 63 && symbols[8] > symbols[2]);
        assert_eq!(
            symbols[0], symbols[3],
            "`let a = 1;` is one shape wherever it stands"
        );
        assert_ne!(symbols[0], symbols[9], "test code is a shape apart");
        let whole = |first: usize, length: usize| {
            (corpus.holding(first, length)).and_then(|it| it.body.as_deref())
        };
        assert_eq!(whole(0, 2), Some("whole"));
        assert_eq!((whole(0, 1), whole(2, 2)), (None, None));
        // `a` is read after the run; `b` is only bound again.
        assert_eq!(corpus.outliving(2, 2), 1);
        assert_eq!(corpus.outliving(0, 1), 1);
        assert_eq!(
            corpus.outliving(5, 2),
            0,
            "a run that ends its block has nothing after it"
        );
        assert_eq!(corpus.outliving(7, 1), 0, "the last block ends the corpus");
        let differs = corpus
            .differs(&[&corpus.units[0..2], &corpus.units[2..4]], None)
            .unwrap();
        let shown: Vec<String> = differs.iter().map(|tuple| corpus.shown(tuple)).collect();
        assert_eq!(shown, ["`a` / `2`"]);
        let around = corpus.differs(&[&corpus.units[0..2], &corpus.units[2..4]], Some(1));
        assert_eq!(
            around,
            Some(Vec::new()),
            "the unit at the gap is left to each copy"
        );
        let repeated = corpus.ids(["x", "x", "y"].map(String::from).to_vec());
        assert_eq!(corpus.shown(&repeated), "`x` / `y`");
        let many = corpus.ids((1..=6).map(|it| it.to_string()).collect());
        assert_eq!(corpus.shown(&many), "`1` / `2` / `3` / `4` and 2 more");
        let long = corpus.ids(vec!["a".repeat(30)]);
        assert_eq!(corpus.shown(&long), format!("`{}…`", "a".repeat(24)));
    }

    #[test]
    fn code_that_builds_only_for_tests_repeats_only_among_test_code() {
        let tested = |value: &str| format!("fn t() {{\n{}    t!();\n}}\n", run(value));
        let production = spread(&copies(&["a", "b"]), "");
        for src in [
            format!(
                "{production}#[cfg(test)]\nmod tests {{\n{}}}\n",
                tested("c")
            ),
            format!("{production}#[test]\n{}", tested("c")),
        ] {
            assert_eq!(lib(&src).repeats(), [], "{src}");
        }
        let three = spread(&copies(&["a", "b", "c"]), "");
        let gated = format!("#[cfg(test)]\nmod tests {{\n{three}}}\n");
        assert_eq!(lib(&gated).repeats(), [function(6, [4, 13, 22], 5)]);
        let mut corpus = Corpus::default();
        corpus.add("tests/cli.rs", &three, true);
        let mut found = function(6, [2, 11, 20], 5);
        (found.sites.iter_mut()).for_each(|site| site.file = "tests/cli.rs".to_string());
        assert_eq!(corpus.repeats(), [found]);
        let two = spread(&copies(&["a", "b"]), "");
        let mut apart = Corpus::default();
        apart.add(
            "src/lib.rs",
            &format!("#[cfg(test)]\nmod tests {{\n{two}}}\n"),
            false,
        );
        apart.add("tests/cli.rs", &tested("c"), true);
        assert_eq!(
            apart.repeats(),
            [],
            "unit tests and a `tests` file share no code"
        );
        let crates = [
            "tests/cli.rs",
            "a/tests/b/c.rs",
            "src/tests/mod.rs",
            "src/tests.rs",
            "tests",
            "crates/x/src/lib.rs",
        ];
        let reached = ["tests", "a/tests", "src", "src", "", "crates/x/src"];
        assert_eq!(crates.map(reach), reached);
    }

    #[test]
    fn match_arms_in_unit_tests_and_in_a_tests_file_are_apart() {
        const ARMS: &str = "        Kind::Total => report(total, V),
        Kind::Mean if mean > 0 => report(mean, V),
        Kind::Ratio => report(total / mean, V),
        Kind::Count => report(count + 1, V),
        Kind::Spread => report(high - low, V),
        Kind::Peak => report(high, V).max(1),
        Kind::Floor => report(low, V).min(0),
        _ => skip(kind, V),
";
        let matched = |value: &str| {
            let arms = ARMS.replace('V', &format!("\"{value}\""));
            format!("fn t{value}() {{\n    match kind {{\n{arms}    }}\n}}\n")
        };
        let mut corpus = Corpus::default();
        let units = format!(
            "#[cfg(test)]\nmod tests {{\n{}{}}}\n",
            matched("a"),
            matched("b")
        );
        corpus.add("src/lib.rs", &units, false);
        corpus.add("tests/cli.rs", &matched("c"), true);
        assert_eq!(corpus.repeats(), [], "two copies on each side are too few");
        corpus.add(
            "tests/more.rs",
            &format!("{}{}", matched("d"), matched("e")),
            true,
        );
        let sites: Vec<usize> = corpus.repeats().iter().map(|it| it.sites.len()).collect();
        assert_eq!(sites, [3], "the arms in `tests` are one group of three");
    }

    #[test]
    fn tests_whose_whole_bodies_repeat_are_one_test_over_a_table_of_cases() {
        let cases = |marks: [&str; 3]| -> String {
            (marks.iter().zip(["a", "b", "c"]).enumerate())
                .map(|(at, (mark, value))| format!("#[{mark}]\nfn t{at}() {{\n{}}}\n", run(value)))
                .collect()
        };
        let sites = |file: &str| [3, 12, 21].map(|line| site(file, line, line + 5)).to_vec();
        let differs = function(0, [0; 3], 0).differs;
        let table = Repeat {
            sites: sites("src/lib.rs"),
            merge: Merge::Cases,
            saves: 7,
            differs: differs.clone(),
        };
        assert_eq!(
            lib(&cases(["test", "tokio::test", "test"])).repeats(),
            [table]
        );
        let mut corpus = Corpus::default();
        corpus.add("tests/cli.rs", &cases(["test", "test", "inline"]), true);
        let called = Repeat {
            sites: sites("tests/cli.rs"),
            merge: Merge::Call("t2".to_string()),
            saves: 10,
            differs,
        };
        assert_eq!(corpus.repeats(), [called]);
    }

    #[test]
    fn copies_alike_but_for_one_statement_are_one_function_taking_that_statement_as_a_closure() {
        let head: String = (run("a").lines().take(5))
            .map(|line| format!("{line}\n"))
            .collect();
        let bodies = |gaps: [&str; 3]| -> Vec<String> {
            (gaps.iter().zip(["a", "b", "c"]))
                .map(|(gap, value)| {
                    format!(
                        "{head}    {gap}\n    log(\"{value}\", total, mean);\n    let ratio = mean \
                         * 100 / total;\n    report(ratio, total);\n"
                    )
                })
                .collect()
        };
        let closed = |gaps: [&str; 3]| lib(&spread(&bodies(gaps), "")).repeats();
        let lifted = "if mean > 2 {\n        record(total);\n    }";
        let sites =
            [(2, 10), (14, 22), (26, 36)].map(|(line, last)| site("src/lib.rs", line, last));
        assert_eq!(
            closed(["record(mean, total);", "mean.record();", lifted]),
            [Repeat {
                sites: sites.to_vec(),
                merge: Merge::Closure,
                saves: 8,
                differs: function(0, [0; 3], 0).differs,
            }]
        );
        let left = lifted.replace("record(total)", "return");
        assert_eq!(
            closed(["record(mean, total);", "mean.record();", &left]),
            [],
            "a statement that leaves the function cannot move into a closure"
        );
        let gaps = [
            "record(mean, total);",
            "mean.record();",
            "audit.push(mean);",
        ];
        let whole: String = (bodies(gaps).iter().enumerate())
            .map(|(at, body)| format!("fn f{at}() {{\n{body}}}\n"))
            .collect();
        assert_eq!(
            lib(&whole).repeats(),
            [],
            "whole bodies alike are for `duplication`"
        );
    }

    #[test]
    fn a_function_takes_six_values_that_differ_and_no_more() {
        let saves = |columns| {
            let bodies: Vec<String> = (1..=6).map(|at| report(columns, at)).collect();
            let found = lib(&spread(&bodies, "")).repeats();
            found.iter().map(|it| it.saves).collect::<Vec<u32>>()
        };
        assert_eq!((saves(6), saves(7)), (vec![16], Vec::new()));
    }

    #[test]
    fn copies_that_differ_in_a_format_string_merge_where_each_has_the_same_placeholders() {
        let printed = |values: [&str; 3]| {
            let bodies = values.map(|value| run(value).replace("log(", "println!("));
            lib(&spread(&bodies, "")).repeats()
        };
        assert_eq!(printed(["a", "b", "c"]), [function(6, [2, 11, 20], 5)]);
        assert_eq!(printed(["{}", "b", "c"]), []);
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
        assert_eq!(
            intervals(&[1, 0], &[0, 1]),
            [(1, vec![1, 0])],
            "a run the last suffix shares closes at the end"
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
        repeat.merge = Merge::Closure;
        assert_eq!(
            repeat.fix(),
            "move the 3 copies into one function that takes the statement that differs as a \
             closure, and call it from each copy; that removes 8 lines"
        );
        repeat.differs.push("`x` / `y` / `z`".to_string());
        assert_eq!(
            repeat.fix(),
            "move the 3 copies into one function with a parameter for `x` / `y` / `z` that takes \
             the statement that differs as a closure, and call it from each copy; that removes 8 \
             lines"
        );
        repeat.merge = Merge::Cases;
        assert_eq!(
            repeat.places()[0],
            Place::at("case 1 of 3", "a.rs", 1).through(4)
        );
        assert_eq!(
            repeat.fix(),
            "make the 3 tests one test that loops over a table for `x` / `y` / `z`; that removes \
             8 lines"
        );
    }

    #[test]
    fn a_shape_keeps_keywords_and_macro_names_and_reads_every_other_name_as_a_value() {
        let shaped = |src: &str| {
            let mut hasher = DefaultHasher::new();
            let mut leaves = Vec::new();
            let count = shape(src.parse().unwrap(), Inside::Code, &mut hasher, &mut leaves);
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
        let (cmp, leaves, _) = shaped("x.cmp(y)");
        assert_eq!(leaves, ["x", "y"]);
        assert_ne!(shaped("x.total_cmp(y)").0, cmp);
        assert_eq!(shaped("x.sum::<u64>()").1, ["x", "u64"]);
        assert_eq!(shaped("x.size + x.len").1, ["x", "size", "x", "len"]);
        let marked = |text: &str| format!("{FORMAT}{text}");
        let (_, leaves, _) = shaped(r#"log!("{told}", g("a"), x); g("b");"#);
        let expected = [marked(r#""{}""#), "g".into(), marked(r#""a""#), "x".into()];
        assert_eq!(
            leaves,
            [&expected[..], &["g".into(), r#""b""#.into()]].concat()
        );
        assert_eq!(
            shaped(r#"vec![2, "a"]"#).1,
            ["2".to_string(), marked(r#""a""#)]
        );
        assert_eq!(shaped(r#"g(x, ("a", 2))"#).1, ["g", "x", r#""a""#, "2"]);
        let text = |text: &str| format!("{TEXT}{text}");
        let (_, leaves, _) = shaped(r#"format!("{told} {{x}}", g("a"))"#);
        assert_eq!(leaves, [text(r#""{} {{x}}""#), "g".into(), text(r#""a""#)]);
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
