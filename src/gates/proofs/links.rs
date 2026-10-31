//! The `code:` comments over the steps of a TLA+ model or a Verus proof, which name the code each
//! step models: each must name one item, and an item changed after the model's last commit drifts.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::RangeInclusive;
use std::path::Path;

use crate::exec;
use crate::gates::text::claims;
use crate::project::{self, vcs};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};
use crate::tokens::{self, Lexed, Tok};

mod tree;

use tree::Tree;

/// Fails a `code:` comment in a model or a proof whose item is gone or changed.
pub const GATE: Gate = Gate {
    name: "links",
    about: "a `code:` comment in a TLA+ model or a Verus proof that names no item, names one the \
            tree lacks, or names one that changed after the model's last commit",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "stale link(s)",
    },
};

/// The word that opens a link comment.
const OPENS: &str = "code:";

/// The first word of a link comment that names no code on purpose, and says why after it.
const NONE: &str = "none";

/// The key of a link comment that names no item.
const UNLINKED: &str = "unlinked";

/// The key of a link to a file outside this project: a record read elsewhere cannot hold its path.
const OUTSIDE: &str = "outside";

/// The words that define an item a link can name.
const DEFINES: [&str; 10] = [
    "fn", "struct", "union", "enum", "trait", "type", "const", "static", "mod", "impl",
];

/// Words after a defining word that are not the name it defines.
const NOT_A_NAME: [&str; 9] = [
    "fn", "mut", "dyn", "impl", "unsafe", "async", "extern", "const", "for",
];

/// The first names of paths in the Rust library. A path that starts with one and names nothing in
/// the tree is about Rust, as `process::exit` is, and links nothing.
const RUST: [&str; 40] = [
    "std", "core", "alloc", "process", "fs", "io", "mem", "ptr", "sync", "thread", "env", "fmt",
    "iter", "cmp", "ops", "str", "path", "net", "time", "hash", "Arc", "Rc", "Box", "Vec",
    "String", "Option", "Result", "Some", "None", "Ok", "Err", "Ordering", "HashMap", "BTreeMap",
    "Mutex", "RwLock", "Path", "PathBuf", "File", "Duration",
];

/// One link comment: the `code:` line and the comment lines that go on from it, as one text.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    line: usize,
    text: String,
}

/// One name of code in a link comment: an optional `.rs` file, and the item path after its `::`.
#[derive(Debug, PartialEq, Eq)]
struct Link {
    written: String,
    file: Option<String>,
    path: Vec<String>,
}

impl Link {
    /// Whether its file is outside this project, by the rule a committed record keeps to.
    fn outside(&self) -> bool {
        let file = self.file.as_deref();
        file.is_some_and(|file| project::unportable(std::iter::once(file)).is_some())
    }

    /// The key of a finding on this link.
    fn key(&self) -> &str {
        if self.outside() {
            OUTSIDE
        } else {
            &self.written
        }
    }
}

/// One item a link can name: its path inside its file, the line it starts on, and a hash of its
/// tokens without comments.
#[derive(Debug, PartialEq, Eq)]
struct Defined {
    path: Vec<String>,
    line: usize,
    body: u64,
}

/// One production source and the items it defines.
struct Source {
    file: String,
    module: Vec<String>,
    items: Vec<Defined>,
}

/// What a link names in the tree.
enum Resolved<'t> {
    /// A whole file.
    File,
    /// One item, or the copies of it that `#[cfg]` keeps apart in one file.
    Item(&'t Source, Vec<&'t Defined>),
    /// Nothing, and why.
    Missing(String),
    /// More than one file or item, by place.
    Many(Vec<String>),
    /// A path in the Rust library, which is no link.
    Rust,
}

/// Every model's link comments are read first, so a tree with none reads no Rust source.
fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let models = models(ctx)?;
    if models.is_empty() {
        return Ok(Inspection::default());
    }
    let tree = Tree::read(ctx)?;
    let mut drift = Drift::of(ctx)?;
    let mut found = Vec::new();
    for (model, blocks) in &models {
        for block in blocks {
            found.extend(checked(model, block, &tree, drift.as_mut())?);
        }
    }
    Ok(Inspection::debt(found))
}

/// The link comments of each `.tla` file, and of each Rust file under a `proofs` directory that
/// holds a `verus!` block.
fn models(ctx: &Ctx) -> Result<Vec<(String, Vec<Block>)>, String> {
    let entered = |dir: &str| !project::SKIPPED.contains(&dir);
    let named = |name: &str, _: &Path| {
        Path::new(name)
            .extension()
            .is_some_and(|ext| ext == "tla" || ext == "rs")
    };
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    let mut out = Vec::new();
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        let tla = Path::new(&shown)
            .extension()
            .is_some_and(|ext| ext == "tla");
        if !tla && !shown.split('/').any(|dir| dir == "proofs") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // A Rust file that is not UTF-8 holds no `verus!` block.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData && !tla => continue,
            Err(e) => return Err(format!("{shown}: {e}")),
        };
        let found = match tla {
            true => blocks(&text, "\\*"),
            false if text.contains("verus!") => blocks(&text, "//"),
            false => continue,
        };
        if !found.is_empty() {
            out.push((shown, found));
        }
    }
    Ok(out)
}

/// The link comments of a text whose line comments start with `mark`. One goes on up to a line of
/// code, an empty comment, a new `code:` or another label, such as `bad:`.
fn blocks(text: &str, mark: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut open: Option<Block> = None;
    for (at, line) in text.lines().enumerate() {
        let said = comment(line, mark);
        if let Some(text) = said.and_then(|said| said.strip_prefix(OPENS)) {
            let text = text.trim().to_string();
            out.extend(open.replace(Block { line: at + 1, text }));
            continue;
        }
        let more = said.filter(|said| !said.is_empty() && !is_label(said));
        match (open.as_mut(), more) {
            (Some(block), Some(said)) => {
                block.text.push(' ');
                block.text.push_str(said);
            }
            _ => out.extend(open.take()),
        }
    }
    out.extend(open);
    out
}

/// The text of a line comment, `///` and `//!` ones too.
fn comment<'t>(line: &'t str, mark: &str) -> Option<&'t str> {
    let rest = line.trim_start().strip_prefix(mark)?;
    Some(rest.trim_start_matches(['/', '!']).trim())
}

/// A comment that opens with a lower-case word and a colon starts something new.
fn is_label(said: &str) -> bool {
    let word = said.split_whitespace().next().unwrap_or("");
    let lower = |name: &str| {
        name.chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
    };
    word.strip_suffix(':')
        .is_some_and(|name| !name.is_empty() && lower(name))
}

/// The findings of one link comment: no link in it, a link to nothing or to more than one item,
/// and an item that changed after the model's last commit.
fn checked(
    model: &str,
    block: &Block,
    tree: &Tree,
    mut drift: Option<&mut Drift<'_>>,
) -> Result<Vec<Finding>, String> {
    let line = u32::try_from(block.line).unwrap_or(u32::MAX);
    let at = |item: &str, message: String| Finding::at(model, &message).line(line).item(item);
    let first = block.text.split(|c: char| !c.is_alphanumeric()).next();
    if first.is_some_and(|word| word.eq_ignore_ascii_case(NONE)) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    let mut linked = false;
    for link in links(&block.text) {
        let resolved = tree.resolve(&link, model);
        linked |= !matches!(resolved, Resolved::Rust);
        if let Some(message) = wrong(&link, resolved, model, drift.as_deref_mut())? {
            found.push(at(link.key(), message));
        }
    }
    if !linked {
        let message = "this `code:` comment names no item; write `file.rs::Type::fn` or \
                       `Type::fn` outside parentheses, or start it with `none;` and say why";
        found.push(at(UNLINKED, message.to_string()));
    }
    Ok(found)
}

/// What is wrong with one link, if anything.
fn wrong(
    link: &Link,
    resolved: Resolved<'_>,
    model: &str,
    drift: Option<&mut Drift<'_>>,
) -> Result<Option<String>, String> {
    let written = &link.written;
    let message = match resolved {
        Resolved::Rust | Resolved::File => return Ok(None),
        Resolved::Item(source, copies) => {
            let Some(drift) = drift else {
                return Ok(None);
            };
            let Some(commit) = drift.since(model, source, &copies)? else {
                return Ok(None);
            };
            format!(
                "`{written}` changed after {commit}, this model's last commit; check that the \
                 model still says what the code does, then commit the model"
            )
        }
        Resolved::Missing(why) => format!("`{written}` names nothing: {why}"),
        Resolved::Many(places) => {
            let shown = places
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let path = link.path.join("::");
            format!(
                "`{written}` names {} items ({shown}); write more of its path, or its file as \
                 `file.rs::{path}`",
                places.len()
            )
        }
    };
    Ok(Some(message))
}

/// Each word outside parentheses and backticks that names a `.rs` file, with `::path` or not, or a
/// path of two names or more. A file word just before a path word limits the path to that file.
fn links(text: &str) -> Vec<Link> {
    // Each character inside parentheses or backticks, and the marks, is made a `+`, which ends a
    // run of words.
    let (mut depth, mut quoted) = (0_usize, false);
    let mut hide = |c: char| {
        let hidden = depth > 0 || quoted || matches!(c, '(' | ')' | '`');
        match c {
            '`' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
        if hidden { '+' } else { c }
    };
    let open: String = text.chars().map(&mut hide).collect();
    open.split('+').flat_map(run_links).collect()
}

/// The links in one run of words, where a `.rs` file word takes the path word after it.
fn run_links(run: &str) -> Vec<Link> {
    let mut out = Vec::new();
    let mut words = run.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let Some(found) = link(word) else {
            continue;
        };
        let file = Path::new(word).extension().is_some_and(|ext| ext == "rs");
        match words.peek().copied().and_then(link) {
            Some(item) if file && item.file.is_none() => {
                words.next();
                out.push(Link {
                    written: format!("{} {}", found.written, item.written),
                    file: found.file,
                    path: item.path,
                });
            }
            _ => out.push(found),
        }
    }
    out
}

fn link(word: &str) -> Option<Link> {
    let word = word
        .trim_start_matches(['[', '"', '\''])
        .trim_end_matches([',', '.', ';', ':', '!', '?', ']', '"', '\'']);
    let (file, rest) = match word.find(".rs") {
        Some(at) => {
            let (file, rest) = word.split_at(at + ".rs".len());
            let pathlike = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '.');
            if !file.chars().all(pathlike) {
                return None;
            }
            (Some(file.to_string()), rest)
        }
        None => (None, word),
    };
    let mut path: Vec<String> = match (&file, rest) {
        (Some(_), "") => Vec::new(),
        (Some(_), rest) => split(rest.strip_prefix("::")?),
        (None, rest) => split(rest),
    };
    if path.first().is_some_and(|name| name == "crate") {
        path.remove(0);
    }
    let named = path.iter().all(|name| is_name(name));
    let enough = file.is_some() || path.len() >= 2;
    let written = word.to_string();
    (named && enough).then_some(Link {
        written,
        file,
        path,
    })
}

fn split(path: &str) -> Vec<String> {
    path.split("::").map(str::to_string).collect()
}

fn is_name(word: &str) -> bool {
    let mut chars = word.chars();
    let first = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_');
    first && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Each item of a source that a link can name, with its path inside the file. A file that does not
/// lex names nothing. Left out: `#[cfg(test)]` code, function bodies, macro bodies except `verus!`.
fn defined(src: &str) -> Vec<Defined> {
    let Ok(lexed) = Lexed::new(src) else {
        return Vec::new();
    };
    let tested = claims::test_only(&lexed);
    let mut out = Vec::new();
    scan(&lexed, 0, &[], &tested, &mut out);
    out
}

/// Records the items from the token at `i` to the end of the group that holds it.
fn scan(
    lexed: &Lexed,
    mut i: usize,
    outer: &[String],
    tested: &[RangeInclusive<usize>],
    out: &mut Vec<Defined>,
) {
    loop {
        let test = tested.iter().find(|span| span.contains(&i));
        i = match (test, lexed.toks.get(i)) {
            (_, None | Some(Tok::Close(_))) => return,
            (Some(span), _) => span.end() + 1,
            // A group no defining word opens: an attribute, a macro call, a visibility.
            (None, Some(Tok::Open(_))) => lexed.pair[i] + 1,
            (None, Some(_)) => item(lexed, i, outer, tested, out).unwrap_or(i + 1),
        };
    }
}

/// Records the item the word at `i` defines, and the items inside it, and gives the token after
/// it; none when the word defines nothing here.
fn item(
    lexed: &Lexed,
    i: usize,
    outer: &[String],
    tested: &[RangeInclusive<usize>],
    out: &mut Vec<Defined>,
) -> Option<usize> {
    let word = lexed.ident(i);
    // A `verus!` block holds items as a module body does, at the path of what holds it.
    if lexed.calls(i, "verus") {
        scan(lexed, i + 3, outer, tested, out);
        return lexed.pair.get(i + 2).map(|close| close + 1);
    }
    let rules = lexed.calls(i, "macro_rules");
    if !DEFINES.contains(&word) && !rules {
        return None;
    }
    let end = lexed.item_end(i);
    let block = (lexed.toks.get(end) == Some(&Tok::Close('}'))).then(|| lexed.pair[end]);
    if word == "impl" {
        if let (Some(open), Some(name)) = (block, implemented(lexed, i, end)) {
            scan(lexed, open + 1, &joined(outer, &name), tested, out);
        }
        return Some(end + 1);
    }
    let at = match word {
        "macro_rules" => i + 2,
        "static" if lexed.ident(i + 1) == "mut" => i + 2,
        _ => i + 1,
    };
    let name = lexed.ident(at);
    // `const fn`, `unsafe impl` and the like: the next word defines the item.
    if name.is_empty() || name == "_" || NOT_A_NAME.contains(&name) {
        return None;
    }
    let path = joined(outer, name);
    let body = hashed(&lexed.toks[i..=end]);
    match (word, block) {
        ("enum", Some(open)) => variants(lexed, open, &path, out),
        ("trait" | "mod", Some(open)) => scan(lexed, open + 1, &path, tested, out),
        _ => {}
    }
    let line = lexed.line[at];
    out.push(Defined { path, line, body });
    Some(end + 1)
}

fn joined(outer: &[String], name: &str) -> Vec<String> {
    let mut path = outer.to_vec();
    path.push(name.to_string());
    path
}

/// The type an `impl` adds to: the last name before its block outside `<>`, and after `for` when
/// there is one.
fn implemented(lexed: &Lexed, at: usize, end: usize) -> Option<String> {
    let open = lexed.pair[end];
    let (mut depth, mut name) = (0_i32, None);
    for j in (at + 1..).take_while(|&j| j != open) {
        match &lexed.toks[j] {
            Tok::Punct('<', _) => depth += 1,
            // The `>` of `->` closes nothing.
            Tok::Punct('>', _) if !lexed.is_punct(j - 1, '-') => depth -= 1,
            Tok::Ident(word) if depth == 0 && word == "where" => break,
            Tok::Ident(word) if depth == 0 && word == "for" => name = None,
            // A lifetime.
            Tok::Ident(_) if lexed.is_punct(j - 1, '\'') => {}
            Tok::Ident(word) if depth == 0 && !NOT_A_NAME.contains(&word.as_str()) => {
                name = Some(word.clone());
            }
            _ => {}
        }
    }
    name
}

/// Each variant of the enum whose block opens at `open`.
fn variants(lexed: &Lexed, open: usize, outer: &[String], out: &mut Vec<Defined>) {
    let mut j = open + 1;
    while !matches!(lexed.toks[j], Tok::Close(_)) {
        let Tok::Ident(name) = &lexed.toks[j] else {
            j = step(lexed, j);
            continue;
        };
        let end = variant_end(lexed, j);
        let (path, line) = (joined(outer, name), lexed.line[j]);
        let body = hashed(&lexed.toks[j..end]);
        out.push(Defined { path, line, body });
        j = end;
    }
}

/// The `,` that ends the variant starting at `j`, or the enum's `}`.
fn variant_end(lexed: &Lexed, mut j: usize) -> usize {
    while !lexed.is_punct(j, ',') && !matches!(lexed.toks[j], Tok::Close(_)) {
        j = step(lexed, j);
    }
    j
}

/// The token after the one at `j`, or after the whole group when `j` opens one.
fn step(lexed: &Lexed, j: usize) -> usize {
    match lexed.toks[j] {
        Tok::Open(_) => lexed.pair[j] + 1,
        _ => j + 1,
    }
}

/// The tokens with their doc comments left out, so an edit of a comment changes no hash.
fn hashed(toks: &[Tok]) -> u64 {
    let mut hasher = DefaultHasher::new();
    tokens::without_docs(toks).0.hash(&mut hasher);
    hasher.finish()
}

/// What each model's last commit held of each linked source, read from git once. A model that git
/// does not hold, or that differs from its last commit, is being written, and nothing of it drifts.
struct Drift<'c> {
    root: &'c Path,
    dirty: Vec<String>,
    commits: BTreeMap<String, Option<String>>,
    then: BTreeMap<(String, String), Option<Vec<Defined>>>,
}

impl<'c> Drift<'c> {
    /// None outside git: a link that cannot drift still has to name one item.
    fn of(ctx: &'c Ctx) -> Result<Option<Self>, String> {
        if ctx.vcs != Some(vcs::Kind::Git) {
            return Ok(None);
        }
        Ok(Some(Self {
            root: &ctx.root,
            dirty: vcs::dirty(&ctx.root, ctx.vcs)?,
            commits: BTreeMap::new(),
            then: BTreeMap::new(),
        }))
    }

    /// The model's last commit, cut short, when no copy of the item reads as it did there.
    fn since(
        &mut self,
        model: &str,
        source: &Source,
        copies: &[&Defined],
    ) -> Result<Option<String>, String> {
        let commit = match self.commits.entry(model.to_string()) {
            Entry::Occupied(it) => it.get().clone(),
            Entry::Vacant(it) => {
                let writing = self.dirty.iter().any(|path| path == model);
                let commit = if writing {
                    None
                } else {
                    last_commit(self.root, model)?
                };
                it.insert(commit).clone()
            }
        };
        let Some(commit) = commit else {
            return Ok(None);
        };
        let then = match self.then.entry((commit.clone(), source.file.clone())) {
            Entry::Occupied(it) => it.into_mut(),
            Entry::Vacant(it) => {
                let text = vcs::text_at(self.root, &commit, &source.file)?;
                it.insert(text.map(|text| defined(&text)))
            }
        };
        let kept = |item: &&Defined| {
            let same = |old: &Defined| old.path == item.path && old.body == item.body;
            then.iter().flatten().any(same)
        };
        Ok((!copies.iter().all(kept)).then(|| commit.chars().take(10).collect()))
    }
}

/// The last commit that changed `model`, or none for a file git does not hold.
fn last_commit(root: &Path, model: &str) -> Result<Option<String>, String> {
    let out = exec::tool(root, "git", &["log", "-1", "--format=%H", "--", model])?;
    if !out.success() {
        let why = out.why_it_failed();
        return Err(format!(
            "git could not read the history of `{model}`: {why}"
        ));
    }
    let commit = out.stdout.trim();
    Ok((!commit.is_empty()).then(|| commit.to_string()))
}

#[cfg(test)]
mod tests {
    use super::tree::module;
    use super::*;
    use crate::testdir::Held;

    fn written(text: &str) -> Vec<String> {
        links(text).into_iter().map(|link| link.written).collect()
    }

    fn paths(src: &str) -> Vec<String> {
        defined(src)
            .into_iter()
            .map(|item| item.path.join("::"))
            .collect()
    }

    fn found(inspection: &Inspection) -> Vec<(u32, &str)> {
        let each = inspection.debt.iter();
        each.map(|it| (it.line.unwrap(), it.item.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn a_link_comment_goes_on_over_the_comment_lines_after_it() {
        let text = "\\* code: client::open; a refused connect\n\\*   is Reply::Absent\nA == 1\n\
                    \\* code: none; the model's own clock\n\\* bad: the mutant\n\
                    \\* code: claim\n\\*\n\\* store.rs::put is prose here\n\\* code:x.rs::f";
        let read = blocks(text, "\\*");
        let lines: Vec<(usize, &str)> = read.iter().map(|it| (it.line, it.text.as_str())).collect();
        assert_eq!(
            lines,
            [
                (1, "client::open; a refused connect is Reply::Absent"),
                (4, "none; the model's own clock"),
                (6, "claim"),
                (9, "x.rs::f"),
            ]
        );
        let rust = "verus! {\n/// code: lib.rs::put\n// and Batch::finish\nfn f() {}\n}\n";
        let text = "lib.rs::put and Batch::finish".to_string();
        assert_eq!(blocks(rust, "//"), [Block { line: 2, text }]);
    }

    #[test]
    fn a_link_is_a_file_or_a_path_outside_parentheses_and_backticks() {
        let text = "client::open; is Reply::Absent: (client::released) `x::y` process::exit, \
                    crates/a/src/b.rs::B::c, b.rs. crate::x::y + 1::2 foo:: c.rsx a.rs:f a+b.rs q@r.rs";
        assert_eq!(
            written(text),
            [
                "client::open",
                "Reply::Absent",
                "process::exit",
                "crates/a/src/b.rs::B::c",
                "b.rs",
                "crate::x::y",
                "b.rs"
            ]
        );
        let link = links("crate::x::y").pop().unwrap();
        let path = vec!["x".to_string(), "y".to_string()];
        assert_eq!((link.file, link.path), (None, path));
        let link = links("b.rs::B::c").pop().unwrap();
        assert_eq!(link.file.as_deref(), Some("b.rs"));
    }

    /// A file word just before a path word limits the path to that file. A mark after the file, a
    /// parenthesis or a `+` between them keeps the two apart. Mutant: read each word alone.
    #[test]
    fn a_file_word_just_before_a_path_limits_the_path_to_that_file() {
        let text =
            "src/a.rs Type::f, b.rs, Type::g c.rs (x) Type::h d.rs + Type::i e.rs::E::j F::k";
        assert_eq!(
            written(text),
            [
                "src/a.rs Type::f",
                "b.rs",
                "Type::g",
                "c.rs",
                "Type::h",
                "d.rs",
                "Type::i",
                "e.rs::E::j",
                "F::k"
            ]
        );
        let link = links("src/a.rs crate::Type::f").pop().unwrap();
        let path = vec!["Type".to_string(), "f".to_string()];
        assert_eq!((link.file.as_deref(), link.path), (Some("src/a.rs"), path));
    }

    /// A link never names an item of the model that holds it, and a file word limits the path
    /// after it. Mutants: drop either rule, and a link names 2 items.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_link_names_one_item_outside_the_model_that_holds_it() {
        let feed = "pub struct Chunked;\nimpl Chunked {\n    pub fn feed(&self) {}\n";
        let first = format!("{feed}    pub fn reset(&mut self) {{}}\n}}\n");
        let model = "verus! {\npub struct Chunked;\nimpl Chunked {\n\
                     \x20   // code: src/http1/chunk.rs Chunked::feed\n    pub fn feed(&self) {}\n\
                     \x20   // code: Chunked::reset\n    pub fn reset(&mut self) {}\n}\n}\n";
        let ctx = Held::tree(
            "gate-links-model",
            &[
                ("src/http1/chunk.rs", first.as_str()),
                ("src/http2/chunk.rs", &format!("{feed}}}\n")),
                ("proofs/verus/chunk.rs", model),
            ],
        );
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(found(&inspection), Vec::<(u32, &str)>::new());
    }

    #[test]
    fn each_item_a_link_can_name_is_found_with_its_path_in_the_file() {
        let src = "pub fn f() { fn inner() {} }\npub struct S(u8);\n\
                   pub enum E {\n  /// doc\n  #[default] A,\n  B(u8, (u8, u8)),\n  C { x: u8 },\n  D = 4,\n}\n\
                   pub trait T { fn t(&self); }\nimpl<X: Fn() -> u8> T for S { fn t(&self) {} }\n\
                   impl<'a> Wrap<'a, u8> where u8: Copy { pub const fn w() {} }\n\
                   mod m { pub(crate) static mut N: u8 = 0; macro_rules! mac { () => {}; } }\n\
                   const _: () = ();\n#[cfg(test)]\nmod tests { fn hidden() {} }\n\
                   pub(crate) fn g() {}\nm! { fn inside() {} }\npub fn after() {}\n";
        let mut found = paths(src);
        found.sort();
        assert_eq!(
            found,
            [
                "E", "E::A", "E::B", "E::C", "E::D", "S", "S::t", "T", "T::t", "Wrap::w", "after",
                "f", "g", "m", "m::N", "m::mac"
            ]
        );
        assert_eq!(paths("fn ( {"), Vec::<String>::new());
    }

    #[test]
    fn the_items_of_a_verus_block_take_the_path_of_what_holds_it() {
        let src = "verus! {\npub fn vouches(same: bool, at: (i64, u32)) -> (r: bool)\n\
                   \x20   requires at.1 < 1_000_000_000,\n    ensures r == same,\n{ same }\n\
                   pub open spec fn law() -> bool { true }\n}\n\
                   mod m { verus! { proof fn lemma() ensures true {} } }\nm! { fn inside() {} }\n";
        let mut found = paths(src);
        found.sort();
        assert_eq!(found, ["law", "m", "m::lemma", "vouches"]);
        assert_eq!(paths("verus!"), Vec::<String>::new());
    }

    #[test]
    fn a_comment_changes_no_hash_and_code_does() {
        let body = |src: &str| defined(src)[0].body;
        let plain = body("pub fn f() -> u8 { 1 }");
        assert_eq!(
            plain,
            body("/// said\npub fn f() -> u8 {\n  // also said\n  1\n}")
        );
        assert_ne!(plain, body("pub fn f() -> u8 { 2 }"));
    }

    #[test]
    fn a_file_s_place_gives_its_module_path() {
        let path = |file: &str| module(file).join("::");
        let batch = "crates/outpost-core/src/object/store/batch.rs";
        assert_eq!(path(batch), "outpost_core::object::store::batch");
        assert_eq!(path("crates/k/src/a/mod.rs"), "k::a");
        assert_eq!(path("src/lib.rs"), "");
        assert_eq!(path("tools/x.rs"), "tools::x");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_link_names_one_item_and_a_comment_with_none_says_why() {
        let model = "---- MODULE M ----\n\
                     \\* code: client::open; a refused connect is Reply::Absent (process::exit)\n\
                     A == 1\n\\* code: claim (`registry.lock()`)\nB == 1\n\
                     \\* code: none; the model's own clock\nC == 1\n\
                     \\* code: store.rs::ObjectStore::batch for the add;\n\
                     \\* crates/b/src/client.rs::gone and put\nD == 1\n\
                     \\* code: Store::vanished, then process::exit\nE == 1\n\
                     \\* code: store::put, then a::store::put and client.rs\nF == 1\n\
                     \\* code: lost.rs and store.rs\n====\n";
        let store = "pub struct ObjectStore;\nimpl ObjectStore { pub fn batch(&self) {} }\n\
                     pub fn put() {}\n";
        let client = "pub fn open() {}\npub enum Reply { Absent, Here(u8) }\n";
        let ctx = Held::tree(
            "gate-links",
            &[
                ("crates/a/src/store.rs", store),
                ("crates/b/src/store.rs", "pub fn put() {}\n"),
                ("crates/b/src/client.rs", client),
                ("proofs/tla/M.tla", model),
                (
                    "proofs/verus/plain.rs",
                    "// code: nothing, as no verus block\n",
                ),
            ],
        );
        std::fs::create_dir_all(ctx.root.join("tests/proofs")).unwrap();
        std::fs::write(ctx.root.join("tests/proofs/raw.rs"), [0xff, 0xfe]).unwrap();
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(
            found(&inspection),
            [
                (4, "unlinked"),
                (8, "crates/b/src/client.rs::gone"),
                (11, "Store::vanished"),
                (13, "store::put"),
                (15, "lost.rs"),
                (15, "store.rs")
            ]
        );
        assert_eq!(inspection.blockers, Vec::new());
        let none = Held::tree("gate-links-none", &[("src/lib.rs", "pub fn f() {}\n")]);
        assert_eq!(inspect(&none).unwrap(), Inspection::default());
        let raw = Held::tree("gate-links-raw", &[("src/lib.rs", "pub fn f() {}\n")]);
        std::fs::write(raw.root.join("Raw.tla"), [0xff, 0xfe]).unwrap();
        assert!(inspect(&raw).unwrap_err().starts_with("Raw.tla: "));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_model_is_a_tla_file_or_a_rust_file_under_proofs_with_a_verus_block() {
        let verus = "verus! {\n// code: gone.rs\nfn f() {}\n}\n";
        let ctx = Held::tree(
            "gate-links-where",
            &[
                ("src/lib.rs", "pub fn f() {}\n"),
                ("src/v.rs", verus),
                ("proofs/verus/plain.rs", "// code: plain.rs\n"),
                ("proofs/verus/q.rs", verus),
                ("specs/N.tla", "\\* code: lost.rs\nA == 1\n"),
                ("tests/proofs/z.rs", verus),
            ],
        );
        // In name order this file comes before `z.rs`, so the walk must go on past it.
        std::fs::write(ctx.root.join("tests/proofs/raw.rs"), [0xff, 0xfe]).unwrap();
        let read = models(&ctx).unwrap();
        let debt: Vec<(&str, &str)> = read
            .iter()
            .map(|(file, blocks)| (file.as_str(), blocks[0].text.as_str()))
            .collect();
        let wanted = [
            ("proofs/verus/q.rs", "gone.rs"),
            ("specs/N.tla", "lost.rs"),
            ("tests/proofs/z.rs", "gone.rs"),
        ];
        assert_eq!(debt, wanted);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_link_to_a_file_names_one_source_or_says_why_not() {
        let lib = "pub fn f() {}\n";
        let files = [("crates/a/src/lib.rs", lib), ("crates/b/src/lib.rs", lib)];
        let ctx = Held::tree("gate-links-files", &files);
        let tree = Tree::read(&ctx).unwrap();
        let resolved = |text: &str| tree.resolve(&links(text)[0], "M.tla");
        let missing = "no source file ends in `gone.rs`";
        assert!(matches!(resolved("gone.rs"), Resolved::Missing(why) if why == missing));
        assert!(matches!(resolved("a/src/lib.rs"), Resolved::File));
        let both = ["crates/a/src/lib.rs", "crates/b/src/lib.rs"];
        assert!(matches!(resolved("lib.rs"), Resolved::Many(files) if files == both));
        let items = |text: &str| -> Vec<String> {
            let block = Block {
                line: 1,
                text: text.into(),
            };
            let found = checked("M.tla", &block, &tree, None).unwrap();
            found.into_iter().filter_map(|it| it.item).collect()
        };
        assert_eq!(items("none; a clock"), Vec::<String>::new());
        assert_eq!(items("a/src/lib.rs::f lib.rs"), ["lib.rs"]);
        assert_eq!(items("prose"), [UNLINKED]);
        assert_eq!(
            items("../b/src/lib.rs::f /crates/a/src/lib.rs"),
            [OUTSIDE, OUTSIDE]
        );
        let outside = |why: &str| why.starts_with("its file is outside this project");
        assert!(matches!(resolved("../a/src/lib.rs::f"), Resolved::Missing(why) if outside(&why)));
    }

    #[test]
    fn an_impl_adds_to_the_type_after_for_and_each_variant_of_an_enum_is_an_item() {
        let lexed = Lexed::new("impl<X: Fn() -> u8> T for S<X> where X: Copy {}").unwrap();
        assert_eq!(implemented(&lexed, 0, lexed.item_end(0)), Some("S".into()));
        let named = |source: &str| {
            let lexed = Lexed::new(source).unwrap();
            implemented(&lexed, 0, lexed.item_end(0))
        };
        assert_eq!(named("impl<T> Wrap<T> {}"), Some("Wrap".into()));
        assert_eq!(named("impl dyn Shape + 'static {}"), Some("Shape".into()));
        assert_eq!(named("impl Display for () {}"), None);
        assert_eq!(named("impl Trait for fn() {}"), None);
        let each = |source: &str| {
            let mut out = Vec::new();
            variants(&Lexed::new(source).unwrap(), 0, &["E".into()], &mut out);
            out.into_iter()
                .map(|it| (it.path.join("::"), it.body))
                .unzip::<_, _, Vec<_>, Vec<_>>()
        };
        let (paths, _) = each("{ #[default] A, B(u8, u16), C = 4 }");
        assert_eq!(paths, ["E::A", "E::B", "E::C"]);
        let (_, alone) = each("{ A }");
        assert_eq!(
            each("{ A, }").1,
            alone,
            "the comma after a variant is not part of it"
        );
        assert_ne!(each("{ B }").1, alone);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run git")]
    fn an_item_that_changed_after_the_model_s_last_commit_drifts_until_the_model_is_committed() {
        let lib = "pub fn f() -> u8 { 1 }\npub fn g() {}\n";
        let model = "\\* code: lib.rs::f and lib.rs::g\n";
        let files = [("src/lib.rs", lib), ("proofs/M.tla", model)];
        let dir = crate::testdir::repo("gate-links-drift", &files);
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut ctx = Ctx::for_root(dir.to_path_buf(), baseline);
        ctx.vcs = Some(vcs::Kind::Git);
        let commit = |what: &str| {
            let who = ["-c", "user.email=t@example.com", "-c", "user.name=t"];
            let args = [&who[..], &["commit", "-qam", what]].concat();
            assert!(exec::run("git", &args, &dir).unwrap().success());
        };
        let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
        assert_eq!(found(&inspect(&ctx).unwrap()), []);
        assert!(last_commit(&dir, "proofs/M.tla").unwrap().is_some());
        assert_eq!(last_commit(&dir, "proofs/Never.tla").unwrap(), None);

        write(
            "src/lib.rs",
            "pub fn f() -> u8 { 2 }\n/// g, now said\npub fn g() {}\n",
        );
        commit("the code moves on");
        let drifted = inspect(&ctx).unwrap();
        assert_eq!(found(&drifted), [(1, "lib.rs::f")]);
        let message = &drifted.debt[0].message;
        assert!(message.contains("changed after"), "{message}");

        write(
            "proofs/M.tla",
            "\\* code: lib.rs::f and lib.rs::g, read again\n",
        );
        assert_eq!(found(&inspect(&ctx).unwrap()), [], "a model being written");
        commit("the model is read again");
        assert_eq!(found(&inspect(&ctx).unwrap()), []);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run git")]
    fn a_model_whose_history_git_cannot_read_is_an_error() {
        let dir = crate::testdir::tree("gate-links-lost", &[(".git", "gitdir: nowhere\n")]);
        let lost = last_commit(&dir, "M.tla").unwrap_err();
        assert!(
            lost.starts_with("git could not read the history of `M.tla`: "),
            "{lost}"
        );
    }
}
