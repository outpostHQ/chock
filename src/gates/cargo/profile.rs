//! The `profile` gate: the release profile takes the wins that cost only compile time, and no
//! manifest or cargo-config setting silences a check for the whole build.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::project::workspace::{Metadata, Package};

use crate::gates::source::NEVER_SHIPPED;
use crate::project;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "profile",
    about: "the release profile takes the wins that cost only compile time, and nothing in the \
            manifest or the cargo config silences a check for the whole build",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

const MANIFEST: &str = "Cargo.toml";

/// Read from the workspace root only, because cargo ignores `[profile]` in a member manifest.
const RELEASE: &str = "profile.release";

/// The profile local builds and tests use, also read from the workspace root.
const DEV: &str = "profile.dev";

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let review = review(&read(ctx)?)?;
    let passed = review.failures.is_empty();
    let mut findings = review.failures;
    findings.extend(review.notes);
    findings.extend(review.dev);
    Ok(Outcome {
        passed,
        findings,
        said: None,
    })
}

/// The gate's findings, split by whether they fail it; notes are only advisory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Review {
    pub failures: Vec<Finding>,
    pub notes: Vec<Finding>,
    /// Advisory notes on the dev profile, kept apart from the release `notes`.
    pub dev: Vec<Finding>,
}

impl Review {
    fn fail(&mut self, finding: Finding) {
        self.failures.push(finding);
    }

    /// Records an advisory, its item prefixed `advisory, ` so no reader takes it for a failure.
    fn note(&mut self, finding: Finding) {
        let setting = finding.item.clone().unwrap_or_default();
        self.notes
            .push(finding.item(&format!("advisory, {setting}")));
    }
}

/// The text of every file the gate reads; `members` is keyed by the path a finding prints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sources {
    pub root: String,
    pub members: BTreeMap<String, String>,
    /// The cargo configuration: the name it was found under, then its text.
    pub config: Option<(String, String)>,
    /// Whether the workspace ships an executable; a library's consumers compile it with their own
    /// release profile.
    pub binary: bool,
}

/// Applies every rule to the gathered text. A file that does not parse is an error, never a pass.
pub fn review(sources: &Sources) -> Result<Review, String> {
    let root = parse(&sources.root).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let mut review = Review::default();
    release_profile(&root, sources.binary, &mut review);
    lint_tables(sources, &root, &mut review)?;
    rustflags(sources, &mut review)?;
    review.dev = dev_profile(&root);
    Ok(review)
}

fn release_profile(doc: &Doc, binary: bool, review: &mut Review) {
    if binary {
        free_wins(doc, review);
        unchecked_overflow(doc, review);
    }
    if full_debug(doc) && !strips(doc) {
        review.fail(about(
            doc,
            "debug",
            "release builds ship full debug information unstripped, so absolute build paths travel inside the binary",
        ));
    }
    advisories(doc, review);
}

/// The three settings that cost only compile time. Advisory, since most projects would trip them.
fn free_wins(doc: &Doc, review: &mut Review) {
    if !strips(doc) {
        review.note(about(
            doc,
            "strip",
            "release builds keep their symbol table, which the binary does not need to run: `strip = true` drops it",
        ));
    }
    if !linked_whole(doc) {
        review.note(about(
            doc,
            "lto",
            "release builds optimise each unit in isolation: `lto = true` lets the linker drop code nothing reaches",
        ));
    }
    if doc.get(&under(RELEASE, "codegen-units")) != Some(&Value::Int(1)) {
        review.note(about(
            doc,
            "codegen-units",
            "release builds are split across 16 codegen units, which hides optimisations from each other: `codegen-units = 1` restores them",
        ));
    }
}

/// Fails only an explicit `overflow-checks = false`, not cargo's default, which is the norm.
fn unchecked_overflow(doc: &Doc, review: &mut Review) {
    if doc.get(&under(RELEASE, "overflow-checks")) == Some(&Value::Bool(false)) {
        review.fail(about(
            doc,
            "overflow-checks",
            "the release profile turns overflow checks off, so integer overflow wraps silently in the shipped binary",
        ));
    }
}

/// Settings that trade speed or behaviour rather than compile time: reported, never enforced.
fn advisories(doc: &Doc, review: &mut Review) {
    if doc.text(&under(RELEASE, "panic")) == Some("abort") {
        review.note(about(
            doc,
            "panic",
            "`panic = \"abort\"` removes unwinding: a panic can no longer be caught and destructors stop running",
        ));
    }
    if matches!(doc.text(&under(RELEASE, "opt-level")), Some("z" | "s")) {
        review.note(about(
            doc,
            "opt-level",
            "the release profile optimises for size rather than speed, which is a trade this gate reports and never enforces",
        ));
    }
}

/// Advice on the dev profile. It changes only build speed, so none of it fails the gate.
fn dev_profile(doc: &Doc) -> Vec<Finding> {
    let mut advice = Review::default();
    // Absent means cargo's dev default, which is full debug information.
    if doc.get(&under(DEV, "debug")).is_none_or(is_full_debug) {
        advice.note(about_in(
            doc,
            DEV,
            "debug",
            "dev builds carry full debug information: `debug = \"line-tables-only\"` keeps panic line numbers and compiles and links faster",
        ));
        if !matches!(
            doc.text(&under(DEV, "split-debuginfo")),
            Some("unpacked" | "packed")
        ) {
            advice.note(about_in(
                doc,
                DEV,
                "split-debuginfo",
                "full debug information passes through the linker on every dev build: `split-debuginfo = \"unpacked\"` leaves it beside the objects",
            ));
        }
    }
    advice.notes
}

/// The directories `[workspace] exclude` keeps out of this workspace, as the manifest writes them.
pub(crate) fn excluded_members(manifest: &str) -> Result<Vec<String>, String> {
    let doc = parse(manifest).map_err(|e| format!("{MANIFEST}: {e}"))?;
    Ok(match doc.get("workspace.exclude") {
        Some(Value::List(dirs)) => dirs
            .iter()
            .map(|dir| dir.trim_end_matches('/').to_string())
            .collect(),
        _ => Vec::new(),
    })
}

/// Where a release-profile finding points: at the setting when it is written, at the section
/// header when it is not, and at the manifest alone when neither is there.
fn about(doc: &Doc, setting: &str, message: &str) -> Finding {
    about_in(doc, RELEASE, setting, message)
}

fn about_in(doc: &Doc, table: &str, setting: &str, message: &str) -> Finding {
    let finding = Finding::at(MANIFEST, message).item(&format!("{table}.{setting}"));
    match doc
        .line(&under(table, setting))
        .or_else(|| doc.tables.get(table).copied())
    {
        Some(line) => finding.line(line),
        None => finding,
    }
}

/// Whether release strips the symbol table; `strip = "debuginfo"` keeps the symbols.
fn strips(doc: &Doc) -> bool {
    match doc.get(&under(RELEASE, "strip")) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Text(mode)) => matches!(mode.as_str(), "symbols" | "true"),
        _ => false,
    }
}

fn linked_whole(doc: &Doc) -> bool {
    match doc.get(&under(RELEASE, "lto")) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Text(mode)) => matches!(mode.as_str(), "fat" | "thin" | "true"),
        _ => false,
    }
}

/// Whether release asks for full debug information (`true`, `2` or `"full"`); lower levels carry
/// only line tables.
fn full_debug(doc: &Doc) -> bool {
    doc.get(&under(RELEASE, "debug")).is_some_and(is_full_debug)
}

fn is_full_debug(value: &Value) -> bool {
    match value {
        Value::Text(name) => name == "full",
        other => matches!(other, Value::Bool(true) | Value::Int(2)),
    }
}

/// One lint namespace: the groups and single rules whose crate-wide `allow` removes a check.
struct Tool {
    name: &'static str,
    groups: &'static [&'static str],
    rules: &'static [&'static str],
}

/// Clippy's default lints that flag a likely bug, plus the three that keep panics out of shipped
/// code. Others, such as `type_complexity`, are a project's own call.
const CLIPPY: Tool = Tool {
    name: "clippy",
    groups: &[
        "all",
        "correctness",
        "suspicious",
        "complexity",
        "perf",
        "style",
    ],
    rules: &[
        "arc_with_non_send_sync",
        "await_holding_lock",
        "await_holding_refcell_ref",
        // Allowed crate-wide, these ship a panic without the per-site reason `source` requires.
        "expect_used",
        "missing_safety_doc",
        "mut_mutex_lock",
        "panic",
        "permissions_set_readonly_false",
        "ptr_arg",
        "redundant_allocation",
        "suspicious_command_arg_space",
        "unwrap_used",
        "zombie_processes",
    ],
};

/// rustc's lint groups, plus `unsafe_code`; allowing any other single lint is a project's own call.
const RUST: Tool = Tool {
    name: "rust",
    groups: &[
        "warnings",
        "future_incompatible",
        "nonstandard_style",
        "unused",
    ],
    rules: &["unsafe_code"],
};

/// The finding for a catalogued rule allowed crate-wide. It names the cost, not who enabled the
/// rule, since some, such as `unwrap_used`, are off by default.
const WHOLE_CRATE: &str =
    "allowed for the whole crate, where an `#[allow]` at the site it fires on would carry a reason";

impl Tool {
    /// The finding for an `allow` on this rule, or `None` outside the catalogue. `-` and `_` are
    /// interchangeable in a lint name.
    fn cost(&self, rule: &str) -> Option<String> {
        let name = rule.replace('-', "_");
        if self.groups.contains(&name.as_str()) {
            return Some(format!(
                "the whole \"{rule}\" group is allowed, which switches off every lint inside it for this crate"
            ));
        }
        self.rules
            .contains(&name.as_str())
            .then(|| WHOLE_CRATE.to_string())
    }
}

/// Judges each member's `[lints]`. A member setting `workspace = true` inherits the root's
/// `[workspace.lints]`, which is judged once, against the root.
fn lint_tables(sources: &Sources, root: &Doc, review: &mut Review) -> Result<(), String> {
    let mut inherited = false;
    for (path, text) in &sources.members {
        let doc = parse(text).map_err(|e| format!("{path}: {e}"))?;
        if doc.get("lints.workspace") == Some(&Value::Bool(true)) {
            inherited = true;
            continue;
        }
        permissive(&doc, "lints", path, review);
    }
    if inherited {
        permissive(root, "workspace.lints", MANIFEST, review);
    }
    Ok(())
}

/// Fails each catalogued lint a table sets to `allow`; `deny` and `warn` silence nothing.
fn permissive(doc: &Doc, table: &str, file: &str, review: &mut Review) {
    for tool in [CLIPPY, RUST] {
        for (rule, level, line) in levels(doc, table, tool.name) {
            let cost = if level == "allow" {
                tool.cost(&rule)
            } else {
                None
            };
            if let Some(message) = cost {
                review.fail(
                    Finding::at(file, &message)
                        .line(line)
                        .item(&format!("{}::{rule}", tool.name)),
                );
            }
        }
    }
}

/// Every level one lint table sets, however the table spells it: a bare level, or the detailed
/// form whose `level` key carries it. `priority` says where a rule applies, not whether.
fn levels(doc: &Doc, table: &str, tool: &str) -> Vec<(String, String, u32)> {
    let prefix = format!("{table}.{tool}.");
    let mut found = Vec::new();
    for (key, (value, line)) in &doc.keys {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        let (rule, field) = rest
            .split_once('.')
            .map_or((rest, None), |(r, f)| (r, Some(f)));
        if let (None | Some("level"), Value::Text(level)) = (field, value) {
            found.push((rule.to_string(), level.clone(), *line));
        }
    }
    found
}

/// Fails flags that switch a check off in the root cargo config's `build` and `target` tables.
/// No config file means no finding.
fn rustflags(sources: &Sources, review: &mut Review) -> Result<(), String> {
    let Some((file, text)) = &sources.config else {
        return Ok(());
    };
    let doc = parse(text).map_err(|e| format!("{file}: {e}"))?;
    for (key, (value, line)) in doc.keys.iter().filter(|(key, _)| carries_flags(key)) {
        for flag in neutralizing(&arguments(value)) {
            let message = format!("\"{flag}\" switches a check off for every build");
            review.fail(Finding::at(file, &message).line(*line).item(key));
        }
    }
    Ok(())
}

fn carries_flags(key: &str) -> bool {
    key == "build.rustflags" || (key.starts_with("target.") && key.ends_with(".rustflags"))
}

/// The flags from a list or one space-separated string, as cargo accepts. Any other shape yields
/// none, as refusing it is cargo's job.
fn arguments(value: &Value) -> Vec<String> {
    match value {
        Value::List(items) => items.clone(),
        Value::Text(joined) => joined.split_whitespace().map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

const CAP_LINTS: &str = "--cap-lints allow";
const ALLOW_WARNINGS: &str = "-A warnings";
const NO_OVERFLOW: &str = "-C overflow-checks=off";

/// The check-silencing flags, each in one canonical spelling. A flag may span one or two arguments.
fn neutralizing(arguments: &[String]) -> Vec<&'static str> {
    let mut matched = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments.get(index).map_or("", String::as_str);
        let next = arguments.get(index + 1).map_or("", String::as_str);
        let (flag, consumed) = neutralized(argument, next);
        if let Some(flag) = flag {
            matched.push(flag);
        }
        index += consumed;
    }
    matched
}

fn neutralized(argument: &str, next: &str) -> (Option<&'static str>, usize) {
    match argument {
        "--cap-lints" if next == "allow" => (Some(CAP_LINTS), 2),
        "--cap-lints=allow" => (Some(CAP_LINTS), 1),
        "-A" | "--allow" if next == "warnings" => (Some(ALLOW_WARNINGS), 2),
        "-Awarnings" | "--allow=warnings" => (Some(ALLOW_WARNINGS), 1),
        "-A" | "--allow" if never_shipped(next) => (Some(ALLOW_SHIPPED), 2),
        other if allowed_by_name(other).is_some_and(never_shipped) => (Some(ALLOW_SHIPPED), 1),
        "-C" | "--codegen" if unchecks_overflow(next) => (Some(NO_OVERFLOW), 2),
        _ => joined(argument),
    }
}

const ALLOW_SHIPPED: &str =
    "-A of clippy::unwrap_used, clippy::expect_used, clippy::panic or unsafe_code";

/// Whether `source` keeps this lint out of shipped code; a build-wide allow bypasses that.
fn never_shipped(lint: &str) -> bool {
    NEVER_SHIPPED.contains(&lint)
}

/// The lint in a joined allow such as `-Aclippy::panic` or `--allow=clippy::panic`.
fn allowed_by_name(argument: &str) -> Option<&str> {
    argument
        .strip_prefix("-A")
        .or_else(|| argument.strip_prefix("--allow="))
        .filter(|rest| !rest.is_empty())
}

fn joined(argument: &str) -> (Option<&'static str>, usize) {
    let setting = argument
        .strip_prefix("-C")
        .or_else(|| argument.strip_prefix("--codegen="));
    match setting {
        Some(setting) if unchecks_overflow(setting) => (Some(NO_OVERFLOW), 1),
        _ => (None, 1),
    }
}

fn unchecks_overflow(setting: &str) -> bool {
    setting.split_once('=').is_some_and(|(key, value)| {
        key == "overflow-checks" && matches!(value, "off" | "false" | "no" | "n" | "0")
    })
}

/// Whether this package ships an executable. A `publish = false` member is taken as an example or
/// dev helper.
fn ships_a_binary(package: &Package) -> bool {
    let publishable = package.publish.as_ref().is_none_or(|to| !to.is_empty());
    publishable
        && package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(|kind| kind == "bin"))
}

/// Reads the manifests and cargo config. `cargo metadata` names the root, members and binaries but
/// not `[profile]` or `[lints]`, so those come from the files.
fn read(ctx: &Ctx) -> Result<Sources, String> {
    let metadata: Metadata = serde_json::from_str(&project::metadata(&ctx.root)?)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    collect(&metadata)
}

fn collect(metadata: &Metadata) -> Result<Sources, String> {
    let root = Path::new(&metadata.workspace_root);
    let mut sources = Sources {
        root: slurp(&root.join(MANIFEST))?,
        config: cargo_config(root)?,
        ..Sources::default()
    };
    for package in metadata
        .packages
        .iter()
        .filter(|p| metadata.workspace_members.contains(&p.id))
    {
        let manifest = Path::new(&package.manifest_path);
        sources
            .members
            .insert(project::relative(root, manifest), slurp(manifest)?);
        sources.binary |= ships_a_binary(package);
    }
    Ok(sources)
}

/// The root's cargo config: `config.toml` first, then the older name without an extension.
fn cargo_config(root: &Path) -> Result<Option<(String, String)>, String> {
    for name in [".cargo/config.toml", ".cargo/config"] {
        let path = root.join(name);
        if path.is_file() {
            return Ok(Some((name.to_string(), slurp(&path)?)));
        }
    }
    Ok(None)
}

fn slurp(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// One TOML value in the shapes this gate reads; any other legal TOML is `Other`, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Bool(bool),
    Int(i64),
    Text(String),
    List(Vec<String>),
    Table(Vec<(String, Value, u32)>),
    Other,
}

/// Leaf settings by dotted path, with their lines. Flat, so a table header and a dotted key naming
/// the same setting land on one key.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Doc {
    keys: BTreeMap<String, (Value, u32)>,
    tables: BTreeMap<String, u32>,
}

impl Doc {
    fn get(&self, path: &str) -> Option<&Value> {
        self.keys.get(path).map(|(value, _)| value)
    }

    fn line(&self, path: &str) -> Option<u32> {
        self.keys.get(path).map(|(_, line)| *line)
    }

    fn text(&self, path: &str) -> Option<&str> {
        match self.get(path) {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        }
    }
}

fn under(table: &str, key: &str) -> String {
    format!("{table}.{key}")
}

fn join(table: &str, key: &str) -> String {
    if table.is_empty() {
        key.to_string()
    } else {
        under(table, key)
    }
}

/// An inline table is flattened onto the path that carries it, so `release = { lto = true }` and
/// `[profile.release] lto = true` reach the same setting under the same name.
fn insert(doc: &mut Doc, path: &str, value: Value, line: u32) {
    match value {
        Value::Table(entries) => {
            doc.tables.insert(path.to_string(), line);
            for (key, inner, at) in entries {
                insert(doc, &under(path, &key), inner, at);
            }
        }
        leaf => {
            doc.keys.insert(path.to_string(), (leaf, line));
        }
    }
}

/// The top-level tables this gate reads; cargo refuses an array of tables (`[[…]]`) at any of them.
const JUDGED: [&str; 5] = ["profile", "lints", "workspace", "build", "target"];

/// Namespaces cargo defines at every depth, so `[[…]]` anywhere below them is refused. Not
/// `workspace` or `target`: `workspace.metadata` and `target.<cfg>` sub-tables are free-form.
const STRICT: [&str; 3] = ["profile", "lints", "build"];

fn not_a_table_here(path: &str) -> bool {
    JUDGED.contains(&path)
        || STRICT
            .iter()
            .any(|root| path.starts_with(&format!("{root}.")))
}

/// A minimal TOML reader (headers, dotted keys, strings, arrays, inline tables), since chock
/// carries no TOML parser. Whatever it cannot place is an error.
fn parse(text: &str) -> Result<Doc, String> {
    let mut toml = Toml {
        text,
        at: 0,
        line: 1,
    };
    let mut doc = Doc::default();
    let mut table = Some(String::new());
    loop {
        toml.skip_gap();
        match toml.peek() {
            None => return Ok(doc),
            Some(b'[') => table = toml.header(&mut doc)?,
            Some(_) => toml.assignment(&mut doc, table.as_deref())?,
        }
    }
}

struct Toml<'a> {
    text: &'a str,
    at: usize,
    line: u32,
}

impl Toml<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn bump(&mut self) {
        if let Some(byte) = self.peek() {
            self.at += 1;
            if byte == b'\n' {
                self.line += 1;
            }
        }
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.bump();
        Some(byte)
    }

    fn matches(&self, bytes: &[u8]) -> bool {
        self.text
            .as_bytes()
            .get(self.at..self.at.saturating_add(bytes.len()))
            == Some(bytes)
    }

    fn slice(&self, from: usize) -> String {
        self.text.get(from..self.at).unwrap_or_default().to_string()
    }

    fn fault(&self, what: &str) -> String {
        format!("line {}: {what}", self.line)
    }

    /// Whitespace, newlines and comments: everything between one statement and the next, and
    /// everything an array or an inline table may hold between its entries.
    fn skip_gap(&mut self) {
        while let Some(byte) = self.peek() {
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' => self.bump(),
                b'#' => self.skip_comment(),
                _ => return,
            }
        }
    }

    fn skip_comment(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.bump();
        }
    }

    /// Spaces and tabs only. A newline ends a statement, so it is never skipped inside one.
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.bump();
        }
    }

    fn header(&mut self, doc: &mut Doc) -> Result<Option<String>, String> {
        let line = self.line;
        self.bump();
        let doubled = self.peek() == Some(b'[');
        if doubled {
            self.bump();
        }
        let path = self.key()?;
        self.skip_spaces();
        if self.take() != Some(b']') || (doubled && self.take() != Some(b']')) {
            return Err(self.fault("a table header with no closing bracket"));
        }
        let Some(path) = path else {
            return Ok(None);
        };
        if !doubled {
            doc.tables.insert(path.clone(), line);
            return Ok(Some(path));
        }
        if not_a_table_here(&path) {
            return Err(self.fault("an array of tables where a table was expected"));
        }
        Ok(Some(format!("{path}[]")))
    }

    fn assignment(&mut self, doc: &mut Doc, table: Option<&str>) -> Result<(), String> {
        let (key, value, line) = self.entry()?;
        if let (Some(table), Some(key)) = (table, key) {
            insert(doc, &join(table, &key), value, line);
        }
        Ok(())
    }

    fn entry(&mut self) -> Result<(Option<String>, Value, u32), String> {
        let line = self.line;
        let key = self.key()?;
        self.skip_spaces();
        if self.take() != Some(b'=') {
            return Err(self.fault("a key with no value"));
        }
        self.skip_spaces();
        Ok((key, self.value()?, line))
    }

    /// The dotted key, or `None` when a quoted segment holds a dot; every setting this gate reads
    /// is spelled with bare keys.
    fn key(&mut self) -> Result<Option<String>, String> {
        let mut parts = vec![self.segment()?];
        self.skip_spaces();
        while self.peek() == Some(b'.') {
            self.bump();
            self.skip_spaces();
            parts.push(self.segment()?);
            self.skip_spaces();
        }
        Ok(parts
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join(".")))
    }

    /// One key segment, or `None` for a quoted one holding a dot: `["profile.release"]` is not
    /// `[profile.release]`.
    fn segment(&mut self) -> Result<Option<String>, String> {
        if matches!(self.peek(), Some(b'"' | b'\'')) {
            let quoted = self.string()?;
            return Ok((!quoted.contains('.')).then_some(quoted));
        }
        let start = self.at;
        while matches!(
            self.peek(),
            Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
        ) {
            self.bump();
        }
        let bare = self.slice(start);
        if bare.is_empty() {
            return Err(self.fault("a key with no name"));
        }
        Ok(Some(bare))
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"' | b'\'') => Ok(Value::Text(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.inline(),
            Some(_) => self.bare(),
            None => Err(self.fault("a key with no value")),
        }
    }

    /// A quoted string with escapes left as written; every value this gate judges is a plain word.
    fn string(&mut self) -> Result<String, String> {
        let Some(quote) = self.take() else {
            return Err(self.fault("a string that ends the file"));
        };
        if self.matches(&[quote, quote]) {
            self.bump();
            self.bump();
            return self.until(&[quote, quote, quote], true);
        }
        self.until(&[quote], false)
    }

    /// Reads up to `close`, failing on an unclosed string. Only a triple-quoted (`long`) string may
    /// cross a newline.
    fn until(&mut self, close: &[u8], long: bool) -> Result<String, String> {
        let start = self.at;
        while let Some(byte) = self.peek() {
            if self.matches(close) {
                let text = self.slice(start);
                for _ in close {
                    self.bump();
                }
                return Ok(text);
            }
            if byte == b'\n' && !long {
                break;
            }
            if byte == b'\\' && close.first() == Some(&b'"') {
                self.bump();
            }
            self.bump();
        }
        Err(self.fault("a string with no closing quote"))
    }

    /// A list of strings, or `Other` for an array holding anything else; the only lists this gate
    /// reads are flags.
    fn array(&mut self) -> Result<Value, String> {
        self.bump();
        let mut items = Vec::new();
        let mut plain = true;
        loop {
            self.skip_gap();
            match self.peek() {
                None => return Err(self.fault("an array with no closing bracket")),
                Some(b']') => {
                    self.bump();
                    return Ok(if plain {
                        Value::List(items)
                    } else {
                        Value::Other
                    });
                }
                Some(b',') => self.bump(),
                Some(_) => match self.value()? {
                    Value::Text(text) => items.push(text),
                    _ => plain = false,
                },
            }
        }
    }

    fn inline(&mut self) -> Result<Value, String> {
        self.bump();
        let mut entries = Vec::new();
        loop {
            self.skip_gap();
            match self.peek() {
                None => return Err(self.fault("an inline table with no closing brace")),
                Some(b'}') => {
                    self.bump();
                    return Ok(Value::Table(entries));
                }
                Some(b',') => self.bump(),
                Some(_) => {
                    let (key, value, line) = self.entry()?;
                    if let Some(key) = key {
                        entries.push((key, value, line));
                    }
                }
            }
        }
    }

    /// An unquoted value: a boolean, an integer, or `Other` (a float or date). It ends with the
    /// statement, array or inline table.
    fn bare(&mut self) -> Result<Value, String> {
        let start = self.at;
        while !matches!(self.peek(), None | Some(b',' | b']' | b'}' | b'\n' | b'#')) {
            self.bump();
        }
        let token = self.slice(start).trim().to_string();
        if token.is_empty() {
            return Err(self.fault("a key with no value"));
        }
        Ok(match token.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => token
                .replace('_', "")
                .parse::<i64>()
                .map_or(Value::Other, Value::Int),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::project::workspace::Target;

    #[test]
    fn a_rustflag_allowing_a_lint_that_keeps_a_panic_out_is_caught() {
        for lint in NEVER_SHIPPED {
            assert_eq!(
                neutralizing(&["-A".to_string(), lint.to_string()]),
                vec![ALLOW_SHIPPED],
                "{lint}"
            );
            assert_eq!(
                neutralizing(&[format!("-A{lint}")]),
                vec![ALLOW_SHIPPED],
                "{lint}"
            );
            assert_eq!(
                neutralizing(&[format!("--allow={lint}")]),
                vec![ALLOW_SHIPPED],
                "{lint}"
            );
        }
    }

    #[test]
    fn a_rustflag_allowing_an_ordinary_lint_is_left_alone() {
        assert_eq!(
            neutralizing(&["-A".to_string(), "dead_code".to_string()]),
            Vec::<&str>::new()
        );
        assert_eq!(
            neutralizing(&["-Adead_code".to_string()]),
            Vec::<&str>::new()
        );
        assert_eq!(
            neutralizing(&["-C".to_string(), "debuginfo=0".to_string()]),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn an_array_of_tables_in_free_form_metadata_does_not_stop_the_gate() {
        let root = "[workspace]\nmembers = []\n\n\
                    [[workspace.metadata.release.pre-release-replacements]]\nfile = \"README.md\"\n";
        assert!(parse(root).is_ok(), "{:?}", parse(root));
        let per_target = "[package]\nname = \"x\"\n\n\
                          [[target.'cfg(windows)'.example]]\nname = \"with_flags\"\n";
        assert!(parse(per_target).is_ok(), "{:?}", parse(per_target));
    }

    #[test]
    fn an_array_of_tables_where_cargo_wants_a_table_is_still_refused() {
        for wrong in [
            "[[profile]]\nx = 1\n",
            "[[profile.release]]\nlto = true\n",
            "[[lints.clippy]]\nall = \"deny\"\n",
            "[[workspace]]\nmembers = []\n",
            "[[build]]\nrustflags = []\n",
        ] {
            assert!(parse(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn stripping_only_debug_info_does_not_satisfy_the_strip_rule() {
        let doc = parse("[profile.release]\nstrip = \"debuginfo\"\n").unwrap();
        assert!(!strips(&doc));
        assert!(strips(
            &parse("[profile.release]\nstrip = \"symbols\"\n").unwrap()
        ));
        assert!(strips(&parse("[profile.release]\nstrip = true\n").unwrap()));
    }

    const FREE: &str = "[profile.release]\nstrip = true\nlto = true\ncodegen-units = 1\n";

    fn from(root: &str) -> Sources {
        Sources {
            root: root.to_string(),
            binary: true,
            ..Sources::default()
        }
    }

    fn with_lints(member: &str) -> Sources {
        let mut sources = from(FREE);
        sources
            .members
            .insert("Cargo.toml".to_string(), member.to_string());
        sources
    }

    fn with_config(config: &str) -> Sources {
        let mut sources = from(FREE);
        sources.config = Some((".cargo/config.toml".to_string(), config.to_string()));
        sources
    }

    fn failures(sources: &Sources) -> Vec<String> {
        review(sources)
            .unwrap()
            .failures
            .iter()
            .map(Finding::render)
            .collect()
    }

    fn advised(sources: &Sources) -> Vec<String> {
        review(sources)
            .unwrap()
            .notes
            .iter()
            .map(Finding::render)
            .collect()
    }

    /// Every setting the review named, failed or advised, without the `advisory, ` label.
    fn settings(sources: &Sources) -> Vec<String> {
        let read = review(sources).unwrap();
        read.failures
            .iter()
            .chain(read.notes.iter())
            .filter_map(|finding| finding.item.clone())
            .map(|item| item.trim_start_matches("advisory, ").to_string())
            .collect()
    }

    #[test]
    fn declining_a_free_win_is_advised_and_never_failed() {
        let bare = "[package]\nname = \"x\"\n";
        assert_eq!(failures(&from(bare)), Vec::<String>::new());
        assert_eq!(
            settings(&from(bare)),
            [
                "profile.release.strip",
                "profile.release.lto",
                "profile.release.codegen-units"
            ]
        );
    }

    #[test]
    fn a_silenced_check_still_fails_where_a_declined_win_does_not() {
        let failed = failures(&with_lints("[lints.clippy]\nstyle = \"allow\"\n"));
        assert!(
            failed.iter().any(|said| said.contains("style")),
            "{failed:?}"
        );
        assert!(
            advised(&with_lints("[lints.clippy]\nstyle = \"allow\"\n")).is_empty(),
            "FREE takes every win, so there is nothing to advise"
        );
    }

    #[test]
    fn a_release_profile_taking_every_free_win_passes() {
        assert_eq!(failures(&from(FREE)), Vec::<String>::new());
    }

    #[test]
    fn a_release_profile_with_no_strip_is_reported() {
        let manifest = "[profile.release]\nlto = true\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.strip"]);
    }

    #[test]
    fn a_release_profile_that_turns_strip_off_is_reported() {
        let manifest = "[profile.release]\nstrip = false\nlto = true\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.strip"]);
    }

    #[test]
    fn strip_spelled_as_a_mode_rather_than_a_boolean_counts_as_stripping() {
        let manifest = "[profile.release]\nstrip = \"symbols\"\nlto = true\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), Vec::<String>::new());
    }

    #[test]
    fn a_release_profile_with_no_link_time_optimisation_is_reported() {
        let manifest = "[profile.release]\nstrip = true\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.lto"]);
    }

    #[test]
    fn link_time_optimisation_turned_off_by_name_is_reported() {
        let manifest = "[profile.release]\nstrip = true\nlto = \"off\"\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.lto"]);
    }

    #[test]
    fn thin_link_time_optimisation_counts_as_optimising_at_the_link() {
        let manifest = "[profile.release]\nstrip = true\nlto = \"thin\"\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), Vec::<String>::new());
    }

    #[test]
    fn a_release_profile_leaving_the_codegen_units_at_the_default_is_reported() {
        let manifest = "[profile.release]\nstrip = true\nlto = true\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.codegen-units"]);
    }

    #[test]
    fn a_manifest_with_no_release_profile_is_reported_once_per_free_win() {
        assert_eq!(
            settings(&from("[package]\nname = \"demo\"\n")),
            [
                "profile.release.strip",
                "profile.release.lto",
                "profile.release.codegen-units",
            ]
        );
    }

    #[test]
    fn a_free_win_finding_points_at_the_line_that_declined_it() {
        let manifest = "[profile.release]\nstrip = false\nlto = true\ncodegen-units = 1\n";
        assert_eq!(
            advised(&from(manifest))[0],
            "Cargo.toml:2: advisory, profile.release.strip: release builds keep their symbol \
             table, which the binary does not need to run: `strip = true` drops it"
        );
    }

    #[test]
    fn a_free_win_nobody_wrote_points_at_the_section_that_should_have_carried_it() {
        let manifest =
            "[package]\nname = \"demo\"\n\n[profile.release]\nstrip = true\nlto = true\n";
        assert_eq!(
            advised(&from(manifest))[0],
            "Cargo.toml:4: advisory, profile.release.codegen-units: release builds are split \
             across 16 codegen units, which hides optimisations from each other: \
             `codegen-units = 1` restores them"
        );
    }

    #[test]
    fn a_workspace_that_builds_no_binary_is_not_held_to_a_release_profile() {
        let sources = Sources {
            root: "[package]\nname = \"demo\"\n".to_string(),
            ..Sources::default()
        };
        assert_eq!(settings(&sources), Vec::<String>::new());
    }

    #[test]
    fn a_binary_its_own_workspace_declines_to_publish_is_not_one_the_project_ships() {
        let helper = |publish| Package {
            id: "dev".to_string(),
            manifest_path: "dev/Cargo.toml".to_string(),
            targets: vec![Target {
                kind: vec!["bin".to_string()],
                ..Target::default()
            }],
            publish,
            ..Package::default()
        };
        assert!(!ships_a_binary(&helper(Some(Vec::new()))));
        assert!(ships_a_binary(&helper(None)));
        assert!(ships_a_binary(&helper(Some(vec!["crates-io".to_string()]))));
        let library = Package {
            targets: vec![Target {
                kind: vec!["lib".to_string()],
                ..Target::default()
            }],
            ..helper(None)
        };
        assert!(!ships_a_binary(&library));
    }

    #[test]
    fn a_release_profile_that_keeps_debug_symbols_is_reported() {
        let manifest =
            "[profile.release]\nstrip = false\nlto = true\ncodegen-units = 1\ndebug = true\n";
        assert!(
            settings(&from(manifest)).contains(&"profile.release.debug".to_string()),
            "{:?}",
            failures(&from(manifest))
        );
    }

    #[test]
    fn debug_symbols_that_are_stripped_again_are_not_reported() {
        let manifest = format!("{FREE}debug = 2\n");
        assert_eq!(settings(&from(&manifest)), Vec::<String>::new());
    }

    /// The only case reaching `is_full_debug`'s level arm: no stripped or line-table build does.
    #[test]
    fn full_symbols_left_unstripped_carry_build_paths_out_of_the_build() {
        for level in ["2", "true", "\"full\""] {
            let manifest = format!(
                "[profile.release]\ndebug = {level}\nstrip = false\nlto = true\ncodegen-units = 1\n"
            );
            assert!(
                failures(&from(&manifest))
                    .iter()
                    .any(|f| f.contains("build paths")),
                "debug = {level}"
            );
        }
    }

    #[test]
    fn line_table_debug_information_is_not_the_full_symbols_that_carry_build_paths() {
        let manifest =
            "[profile.release]\ndebug = 1\nstrip = false\nlto = true\ncodegen-units = 1\n";
        assert_eq!(settings(&from(manifest)), ["profile.release.strip"]);
    }

    #[test]
    fn debug_named_full_carries_the_symbols_a_narrower_name_does_not() {
        for (name, expected) in [
            (
                "full",
                // `debug` fails and `strip` is only advised, so the failure is named first.
                vec!["profile.release.debug", "profile.release.strip"],
            ),
            ("line-tables-only", vec!["profile.release.strip"]),
        ] {
            let manifest = format!(
                "[profile.release]\nstrip = false\nlto = true\ncodegen-units = 1\ndebug = \"{name}\"\n"
            );
            assert_eq!(settings(&from(&manifest)), expected, "{name}");
        }
    }

    #[test]
    fn a_release_profile_that_turns_overflow_checks_off_is_reported() {
        let manifest = format!("{FREE}overflow-checks = false\n");
        assert_eq!(
            settings(&from(&manifest)),
            ["profile.release.overflow-checks"]
        );
    }

    #[test]
    fn a_release_profile_that_leaves_overflow_checks_unwritten_is_not_reported() {
        assert_eq!(settings(&from(FREE)), Vec::<String>::new());
    }

    #[test]
    fn a_profile_in_a_member_manifest_is_not_reported_because_cargo_ignores_it() {
        let mut sources = from(FREE);
        sources.members.insert(
            "crates/one/Cargo.toml".to_string(),
            "[profile.release]\nstrip = false\n".to_string(),
        );
        assert_eq!(settings(&sources), Vec::<String>::new());
    }

    #[test]
    fn aborting_on_panic_is_an_advisory_and_never_fails_the_gate() {
        let manifest = format!("{FREE}panic = \"abort\"\n");
        let review = review(&from(&manifest)).unwrap();
        assert!(review.failures.is_empty(), "{:?}", review.failures);
        assert_eq!(
            review.notes[0].render(),
            "Cargo.toml:5: advisory, profile.release.panic: `panic = \"abort\"` removes \
             unwinding: a panic can no longer be caught and destructors stop running"
        );
    }

    #[test]
    fn optimising_for_size_is_an_advisory_and_never_fails_the_gate() {
        let manifest = format!("{FREE}opt-level = \"z\"\n");
        let review = review(&from(&manifest)).unwrap();
        assert!(review.failures.is_empty(), "{:?}", review.failures);
        assert!(
            review.notes[0].render().contains("optimises for size"),
            "{:?}",
            review.notes
        );
    }

    #[test]
    fn optimising_for_speed_says_nothing_at_all() {
        let manifest = format!("{FREE}opt-level = 3\n");
        assert!(review(&from(&manifest)).unwrap().notes.is_empty());
    }

    fn dev_advice(root: &str) -> Vec<String> {
        review(&from(root))
            .unwrap()
            .dev
            .iter()
            .filter_map(|finding| finding.item.clone())
            .collect()
    }

    /// No `[profile.dev]` is cargo's default: full debug information, passed through the linker.
    #[test]
    fn a_manifest_without_a_dev_profile_is_advised_both_debug_info_wins() {
        assert_eq!(
            dev_advice(FREE),
            [
                "advisory, profile.dev.debug",
                "advisory, profile.dev.split-debuginfo"
            ]
        );
    }

    #[test]
    fn a_dev_profile_with_line_tables_only_says_nothing() {
        let manifest = "[profile.dev]\ndebug = \"line-tables-only\"\n";
        assert_eq!(dev_advice(manifest), Vec::<String>::new());
    }

    #[test]
    fn full_dev_debug_info_already_split_off_the_link_is_advised_only_to_trim_it() {
        let manifest = "[profile.dev]\ndebug = true\nsplit-debuginfo = \"unpacked\"\n";
        assert_eq!(dev_advice(manifest), ["advisory, profile.dev.debug"]);
    }

    #[test]
    fn debug_info_switched_off_needs_no_advice_whatever_build_scripts_use() {
        let manifest = "[profile.dev]\ndebug = 0\n\n[profile.dev.build-override]\nopt-level = 0\n";
        assert_eq!(dev_advice(manifest), Vec::<String>::new());
    }

    #[test]
    fn dev_advice_points_at_the_setting_line_and_never_fails_the_gate() {
        let manifest = "[profile.dev]\ndebug = 2\n";
        let review = review(&from(manifest)).unwrap();
        assert_eq!(
            review.dev.iter().map(Finding::render).collect::<Vec<_>>(),
            [
                "Cargo.toml:2: advisory, profile.dev.debug: dev builds carry full debug \
                 information: `debug = \"line-tables-only\"` keeps panic line numbers and compiles \
                 and links faster",
                "Cargo.toml:1: advisory, profile.dev.split-debuginfo: full debug information \
                 passes through the linker on every dev build: `split-debuginfo = \"unpacked\"` \
                 leaves it beside the objects",
            ]
        );
        assert!(review.failures.is_empty());
    }

    #[test]
    fn a_lint_table_allowing_a_whole_clippy_group_is_reported() {
        let sources = with_lints("[lints.clippy]\nall = \"allow\"\n");
        assert_eq!(
            failures(&sources)[0],
            "Cargo.toml:2: clippy::all: the whole \"all\" group is allowed, which switches off \
             every lint inside it for this crate"
        );
    }

    #[test]
    fn a_lint_table_allowing_a_catalogued_clippy_rule_is_reported() {
        let sources = with_lints("[lints.clippy]\nptr_arg = \"allow\"\n");
        assert_eq!(settings(&sources), ["clippy::ptr_arg"]);
    }

    #[test]
    fn a_rule_allowed_for_the_whole_crate_is_reported_at_its_line_with_what_it_costs() {
        let sources = with_lints(
            "[lints.rust]\nunsafe_code = \"allow\"\n[lints.clippy]\nunwrap_used = \"allow\"\n",
        );
        assert_eq!(
            failures(&sources),
            [
                "Cargo.toml:4: clippy::unwrap_used: allowed for the whole crate, where an \
                 `#[allow]` at the site it fires on would carry a reason",
                "Cargo.toml:2: rust::unsafe_code: allowed for the whole crate, where an \
                 `#[allow]` at the site it fires on would carry a reason",
            ]
        );
    }

    #[test]
    fn a_rule_outside_the_catalogue_is_a_projects_own_call_and_is_left_alone() {
        let sources = with_lints("[lints.clippy]\ntype_complexity = \"allow\"\n");
        assert_eq!(settings(&sources), Vec::<String>::new());
    }

    #[test]
    fn a_lint_set_to_deny_or_to_warn_silences_nothing_and_is_not_reported() {
        let sources = with_lints("[lints.clippy]\nall = \"deny\"\nptr_arg = \"warn\"\n");
        assert_eq!(settings(&sources), Vec::<String>::new());
    }

    #[test]
    fn the_detailed_spelling_of_a_lint_entry_is_read_through_its_level() {
        let sources = with_lints("[lints.clippy]\nall = { level = \"allow\", priority = -1 }\n");
        assert_eq!(settings(&sources), ["clippy::all"]);
    }

    #[test]
    fn a_lint_named_with_hyphens_is_the_same_rule_as_one_named_with_underscores() {
        let sources = with_lints("[lints.rust]\nfuture-incompatible = \"allow\"\n");
        assert_eq!(settings(&sources), ["rust::future-incompatible"]);
    }

    #[test]
    fn allowing_every_rustc_warning_is_reported() {
        let sources = with_lints("[lints.rust]\nwarnings = \"allow\"\n");
        assert_eq!(settings(&sources), ["rust::warnings"]);
    }

    #[test]
    fn a_member_inheriting_the_workspace_lints_is_judged_once_at_the_root() {
        let mut sources = from(&format!(
            "{FREE}\n[workspace.lints.clippy]\nall = \"allow\"\n"
        ));
        for member in ["crates/one/Cargo.toml", "crates/two/Cargo.toml"] {
            sources.members.insert(
                member.to_string(),
                "[lints]\nworkspace = true\n".to_string(),
            );
        }
        assert_eq!(settings(&sources), ["clippy::all"]);
        assert_eq!(failures(&sources)[0].split(':').next(), Some("Cargo.toml"));
    }

    #[test]
    fn a_workspace_lint_table_nobody_inherits_is_not_judged() {
        let sources = from(&format!(
            "{FREE}\n[workspace.lints.clippy]\nall = \"allow\"\n"
        ));
        assert_eq!(settings(&sources), Vec::<String>::new());
    }

    #[test]
    fn a_member_that_does_not_inherit_is_judged_under_its_own_path() {
        let mut sources = from(FREE);
        sources.members.insert(
            "crates/one/Cargo.toml".to_string(),
            "[lints.clippy]\nall = \"allow\"\n".to_string(),
        );
        assert_eq!(
            failures(&sources)[0].split(':').next(),
            Some("crates/one/Cargo.toml")
        );
    }

    #[test]
    fn a_member_that_inherits_does_not_stop_the_one_behind_it_being_judged() {
        let mut sources = from(FREE);
        sources.members.insert(
            "crates/one/Cargo.toml".to_string(),
            "[lints]\nworkspace = true\n".to_string(),
        );
        sources.members.insert(
            "crates/two/Cargo.toml".to_string(),
            "[lints.clippy]\nall = \"allow\"\n".to_string(),
        );
        assert_eq!(settings(&sources), ["clippy::all"]);
        assert_eq!(
            failures(&sources)[0].split(':').next(),
            Some("crates/two/Cargo.toml")
        );
    }

    #[test]
    fn rustflags_capping_every_lint_are_reported() {
        let sources = with_config("[build]\nrustflags = [\"--cap-lints\", \"allow\"]\n");
        assert_eq!(
            failures(&sources)[0],
            ".cargo/config.toml:2: build.rustflags: \"--cap-lints allow\" switches a check off \
             for every build"
        );
    }

    #[test]
    fn rustflags_allowing_every_warning_are_reported_in_either_spelling() {
        for flags in [
            "[\"-A\", \"warnings\"]",
            "\"-Awarnings\"",
            "\"--allow=warnings\"",
        ] {
            let sources = with_config(&format!("[build]\nrustflags = {flags}\n"));
            assert!(
                failures(&sources)[0].contains("\"-A warnings\""),
                "{flags}: {:?}",
                failures(&sources)
            );
        }
    }

    #[test]
    fn rustflags_turning_overflow_checks_off_are_reported_however_they_are_spelled() {
        for flags in [
            "[\"-C\", \"overflow-checks=off\"]",
            "[\"-Coverflow-checks=no\"]",
            "[\"--codegen=overflow-checks=0\"]",
        ] {
            let sources = with_config(&format!("[build]\nrustflags = {flags}\n"));
            assert!(
                failures(&sources)[0].contains("\"-C overflow-checks=off\""),
                "{flags}: {:?}",
                failures(&sources)
            );
        }
    }

    #[test]
    fn a_rustflag_outside_the_closed_list_is_left_alone() {
        let sources = with_config("[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\n");
        assert_eq!(failures(&sources), Vec::<String>::new());
    }

    #[test]
    fn rustflags_under_a_target_table_are_read_as_well_as_the_build_table() {
        let config = "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"-Awarnings\"]\n";
        assert_eq!(
            settings(&with_config(config)),
            ["target.x86_64-unknown-linux-gnu.rustflags"]
        );
    }

    #[test]
    fn a_rustflags_array_spanning_several_lines_is_read_whole() {
        let config = "[build]\nrustflags = [\n  \"-D\",\n  \"warnings\",\n  \"-Awarnings\",\n]\n";
        assert_eq!(settings(&with_config(config)), ["build.rustflags"]);
    }

    #[test]
    fn rustflags_written_as_one_string_are_split_into_arguments() {
        let sources = with_config("[build]\nrustflags = \"-D dead_code --cap-lints allow\"\n");
        assert!(
            failures(&sources)[0].contains("\"--cap-lints allow\""),
            "{:?}",
            failures(&sources)
        );
    }

    #[test]
    fn a_project_with_no_cargo_configuration_is_silent_rather_than_unreadable() {
        assert_eq!(failures(&from(FREE)), Vec::<String>::new());
    }

    #[test]
    fn a_manifest_that_is_not_toml_is_refused_rather_than_read_as_clean() {
        let error = review(&from("[profile.release\nstrip = true\n")).unwrap_err();
        assert_eq!(
            error,
            "Cargo.toml: line 2: a table header with no closing bracket"
        );
    }

    #[test]
    fn a_member_manifest_that_is_not_toml_is_refused_under_its_own_path() {
        let mut sources = from(FREE);
        sources
            .members
            .insert("crates/one/Cargo.toml".to_string(), "name = \n".to_string());
        assert!(
            review(&sources)
                .unwrap_err()
                .starts_with("crates/one/Cargo.toml: "),
            "{:?}",
            review(&sources)
        );
    }

    #[test]
    fn a_cargo_configuration_that_is_not_toml_is_refused_rather_than_read_as_clean() {
        let sources = with_config("[build]\nrustflags = [\"-Awarnings\"\n");
        assert_eq!(
            review(&sources).unwrap_err(),
            ".cargo/config.toml: line 3: an array with no closing bracket"
        );
    }

    #[test]
    fn a_string_that_never_closes_is_refused_rather_than_swallowing_the_file() {
        let manifest = "[profile.release]\nstrip = \"symbols\nlto = true\n";
        assert!(parse(manifest).is_err(), "{:?}", parse(manifest));
    }

    #[test]
    fn a_comment_that_looks_like_a_table_header_is_not_one() {
        let manifest = "# [profile.release]\n[package]\nname = \"demo\"\n";
        assert_eq!(parse(manifest).unwrap().tables.get("profile.release"), None);
    }

    #[test]
    fn a_multi_line_string_holding_a_table_header_is_not_read_as_one() {
        let manifest =
            "[package]\ndescription = \"\"\"\n[profile.release]\nstrip = false\n\"\"\"\n";
        let doc = parse(manifest).unwrap();
        assert_eq!(doc.tables.get("profile.release"), None);
        assert!(doc.get("package.description").is_some());
    }

    #[test]
    fn a_dotted_key_reaches_the_same_setting_as_a_table_header_would() {
        let doc = parse("profile.release.lto = true\n").unwrap();
        assert_eq!(doc.get("profile.release.lto"), Some(&Value::Bool(true)));
    }

    #[test]
    fn an_inline_table_reaches_the_same_setting_as_a_table_header_would() {
        let manifest = "[profile]\nrelease = { strip = true, lto = true, codegen-units = 1 }\n";
        assert_eq!(settings(&from(manifest)), Vec::<String>::new());
    }

    #[test]
    fn an_array_of_tables_where_a_profile_belongs_is_refused() {
        assert!(parse("[[profile.release]]\nstrip = true\n").is_err());
    }

    #[test]
    fn an_array_of_tables_the_gate_never_reads_is_carried_without_complaint() {
        let manifest = format!("{FREE}\n[[bin]]\nname = \"demo\"\n");
        assert_eq!(settings(&from(&manifest)), Vec::<String>::new());
    }

    #[test]
    fn a_quoted_key_holding_a_dot_is_not_split_into_two() {
        let doc = parse("[target.\"a.b\"]\nrustflags = []\n").unwrap();
        assert_eq!(doc.get("target.a.b.rustflags"), None);
        assert_eq!(doc.tables.get("target.a.b"), None);
    }

    /// A manifest that overrides a git dependency carries a `[patch."https://…"]` table.
    #[test]
    fn a_table_this_gate_cannot_address_does_not_stop_it_reading_the_rest() {
        let manifest = "[patch.\"https://github.com/o/p\"]\n\
                        thing = { path = \"src/thing\" }\n\
                        [profile.release]\n\
                        lto = true\n";
        let doc = parse(manifest).unwrap();
        assert_eq!(doc.get("profile.release.lto"), Some(&Value::Bool(true)));
    }

    #[test]
    fn a_quoted_profile_table_is_not_read_as_the_profile_of_that_name() {
        let doc = parse("[\"profile.release\"]\nlto = true\n").unwrap();
        assert_eq!(doc.get("profile.release.lto"), None);
        assert_eq!(doc.tables.get("profile.release"), None);
    }

    #[test]
    fn an_inline_entry_this_gate_cannot_address_leaves_its_neighbours_alone() {
        let doc = parse("[profile.release]\nx = { \"a.b\" = 1, lto = true }\n").unwrap();
        assert_eq!(doc.get("profile.release.x.lto"), Some(&Value::Bool(true)));
        assert_eq!(doc.get("profile.release.x.a.b"), None);
    }

    #[test]
    fn a_comment_after_a_value_is_not_part_of_it() {
        let doc = parse("[profile.release]\ncodegen-units = 1 # one unit\n").unwrap();
        assert_eq!(
            doc.get("profile.release.codegen-units"),
            Some(&Value::Int(1))
        );
    }
}
