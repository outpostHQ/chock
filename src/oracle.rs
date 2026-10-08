//! `chock oracle`: two builds of one program run the same scenarios, and each answer is compared.
//! Equal output, exit codes and files are the evidence a redesign needs before the old code goes.

mod side;

use std::collections::{BTreeMap, BTreeSet};
// Off unix a link is copied as the file it names.
#[cfg(not(unix))]
use std::fs::copy as linked;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use side::Side;

/// The command line of `chock oracle`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Asked<'a> {
    pub old: &'a str,
    pub new: &'a str,
    pub corpus: &'a str,
    pub fixture: Option<&'a str>,
    pub probes: Vec<&'a str>,
    pub normalize: Option<&'a str>,
    pub allow: Option<&'a str>,
    pub timeout: u64,
}

impl<'a> Asked<'a> {
    fn set(&mut self, flag: &str, value: &'a str) -> Result<(), String> {
        match flag {
            "--old" => self.old = value,
            "--new" => self.new = value,
            "--corpus" => self.corpus = value,
            "--fixture" => self.fixture = Some(value),
            "--probe" => self.probes.push(value),
            "--normalize" => self.normalize = Some(value),
            "--allow" => self.allow = Some(value),
            "--timeout" => self.timeout = seconds(value)?,
            _ => return Err(format!("oracle does not take `{flag}`")),
        }
        Ok(())
    }
}

fn seconds(value: &str) -> Result<u64, String> {
    let read = value.parse().ok().filter(|limit| *limit > 0);
    read.ok_or_else(|| "oracle: `--timeout` takes a number of seconds above zero".to_string())
}

/// Each flag with the value after it; the two builds and the corpus are required.
pub fn asked<'a>(args: &[&'a str]) -> Result<Asked<'a>, String> {
    let mut asked = Asked {
        timeout: 60,
        ..Asked::default()
    };
    let mut rest = args.iter().copied();
    while let Some(flag) = rest.next() {
        let value = rest.next();
        asked.set(
            flag,
            value.ok_or_else(|| format!("oracle: `{flag}` takes a value"))?,
        )?;
    }
    let needed = [
        ("--old", asked.old),
        ("--new", asked.new),
        ("--corpus", asked.corpus),
    ];
    match needed.iter().find(|(_, value)| value.is_empty()) {
        Some((flag, _)) => Err(format!("oracle needs `{flag}`")),
        None => Ok(asked),
    }
}

/// One line of the corpus: the commands a build runs in order, in one copy of the fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    name: String,
    steps: Vec<Vec<String>>,
    #[serde(default)]
    stdin: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// One line of the normalize file: text that two honest runs print differently, and its stand-in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    min: usize,
    token: String,
}

/// One line of the allow file: a difference a person accepted, and why.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Allowed {
    scenario: String,
    field: String,
    reason: String,
}

/// Each line of a JSON-lines file; a line that does not read is an error with its number.
fn lines<T: serde::de::DeserializeOwned>(path: &str) -> Result<Vec<T>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    (text.lines().enumerate())
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| serde_json::from_str(line).map_err(|e| format!("{path}:{}: {e}", n + 1)))
        .collect()
}

/// The lines of a file the caller may leave out.
fn optional<T: serde::de::DeserializeOwned>(path: Option<&str>) -> Result<Vec<T>, String> {
    path.map_or_else(|| Ok(Vec::new()), lines)
}

/// What is wrong with a rule chock cannot apply.
fn refused(rule: &Rule) -> Option<String> {
    match (rule.kind.as_str(), rule.text.is_empty(), rule.min) {
        ("text", false, _) | ("hex" | "digits", _, 1..) => None,
        ("text", ..) => Some("a `text` rule needs `text`".to_string()),
        ("hex" | "digits", ..) => Some(format!("a `{}` rule needs `min` above zero", rule.kind)),
        (kind, ..) => Some(format!("no rule kind `{kind}`: text, hex or digits")),
    }
}

/// `text` with each run of `min` or more characters that `is` accepts replaced by `token`.
fn runs(text: &str, min: usize, token: &str, is: fn(&char) -> bool) -> (String, usize) {
    let (mut out, mut hits, mut rest) = (String::new(), 0, text);
    while let Some(first) = rest.chars().next() {
        let run: usize = rest.chars().take_while(is).map(char::len_utf8).sum();
        let (kept, next) = rest.split_at(run.max(first.len_utf8()));
        out.push_str(if run >= min { token } else { kept });
        hits += usize::from(run >= min);
        rest = next;
    }
    (out, hits)
}

/// One rule applied: the new text, and how many places it changed.
fn applied(rule: &Rule, text: &str) -> (String, usize) {
    match rule.kind.as_str() {
        "hex" => runs(text, rule.min, &rule.token, char::is_ascii_hexdigit),
        "digits" => runs(text, rule.min, &rule.token, char::is_ascii_digit),
        _ => (
            text.replace(&rule.text, &rule.token),
            text.matches(&rule.text).count(),
        ),
    }
}

/// A rule that replaces one literal text.
fn literal(text: String, token: &str) -> Rule {
    Rule {
        kind: "text".to_string(),
        text,
        min: 0,
        token: token.to_string(),
    }
}

/// Directories that hold a version store. Their bytes differ between two honest runs.
const STORES: [&str; 2] = [".git", ".outpost"];

/// The exit a command gets when chock had to kill it. It never compares equal.
const TIMEOUT: &str = "timeout";

/// The most of one value a report quotes.
const QUOTED: usize = 4000;

/// Counts the runs of this process, so two of them never share a scratch directory.
static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A run, read and checked before any scenario starts.
struct Plan {
    old: PathBuf,
    new: PathBuf,
    corpus: Vec<Scenario>,
    fixture: Option<PathBuf>,
    probes: Vec<Vec<String>>,
    rules: Vec<Rule>,
    allowed: Vec<Allowed>,
    limit: Duration,
    scratch: PathBuf,
    hits: Mutex<BTreeMap<String, usize>>,
}

/// The absolute path of a build, which must be a file: each command runs in another directory.
fn build(path: &str) -> Result<PathBuf, String> {
    let whole = std::path::absolute(path).map_err(|e| format!("cannot find {path}: {e}"))?;
    match whole.is_file() {
        true => Ok(whole),
        false => Err(format!("{path} is not a file")),
    }
}

impl Plan {
    fn of(asked: &Asked) -> Result<Self, String> {
        let (old, new) = (build(asked.old)?, build(asked.new)?);
        let rules: Vec<Rule> = optional(asked.normalize)?;
        if let Some(why) = rules.iter().find_map(refused) {
            return Err(format!("{}: {why}", asked.normalize.unwrap_or_default()));
        }
        let fixture = asked.fixture.map(PathBuf::from);
        // outpost: ignore[path-is-dir-follows-symlinks] a link the user names is its directory.
        if let Some(dir) = fixture.as_ref().filter(|dir| !dir.is_dir()) {
            return Err(format!("the fixture {} is not a directory", dir.display()));
        }
        let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scratch =
            std::env::temp_dir().join(format!("chock-oracle-{}-{run}", std::process::id()));
        let allowed = optional(asked.allow)?;
        let corpus: Vec<Scenario> = lines(asked.corpus)?;
        if corpus.is_empty() {
            return Err(format!("{} holds no scenario", asked.corpus));
        }
        Ok(Self {
            old,
            new,
            corpus,
            fixture,
            probes: (asked.probes.iter())
                .map(|probe| probe.split_whitespace().map(str::to_string).collect())
                .collect(),
            rules,
            allowed,
            limit: Duration::from_secs(asked.timeout),
            scratch,
            hits: Mutex::default(),
        })
    }

    /// `text` with this side's own directory and each rule's matches replaced by their tokens.
    fn normal(&self, text: &str, own: &[Rule]) -> String {
        let mut text = text.to_string();
        for rule in own.iter().chain(&self.rules) {
            let (next, hits) = applied(rule, &text);
            if let Ok(mut all) = self.hits.lock() {
                *all.entry(rule.token.clone()).or_default() += hits;
            }
            text = next;
        }
        text
    }
}

/// What one command answered, with the text that may differ already replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Answer {
    exit: String,
    stdout: String,
    stderr: String,
}

/// All one build answered for one scenario.
#[derive(Debug, Default)]
struct Ran {
    steps: Vec<Answer>,
    probes: Vec<Answer>,
    tree: BTreeMap<String, u64>,
}

/// A copy of `from` at `to`: directories, files with their permissions, and links as links.
fn copied(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("cannot copy {}: {e}", from.display());
    std::fs::create_dir_all(to).map_err(failed)?;
    for entry in std::fs::read_dir(from).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(failed)?;
        if kind.is_dir() {
            copied(&source, &target)?;
        } else if kind.is_symlink() {
            linked(&source, &target).map_err(failed)?;
        } else {
            std::fs::copy(&source, &target).map_err(failed)?;
        }
    }
    Ok(())
}

/// A link at `target` to what the link at `source` names.
#[cfg(unix)]
fn linked(source: &Path, target: &Path) -> std::io::Result<u64> {
    let names = std::fs::read_link(source)?;
    std::os::unix::fs::symlink(names, target).map(|()| 0)
}

/// The bits that let a file run. Off unix no file has them.
#[cfg(unix)]
fn run_bits(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111
}

#[cfg(not(unix))]
fn run_bits(_: &std::fs::Metadata) -> u32 {
    0
}

/// A hash of one entry: a link's target, a directory as such, or a file's bytes and the bits
/// that let it run.
fn held(path: &Path) -> std::io::Result<(bool, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    let mut hasher = std::hash::DefaultHasher::new();
    if meta.is_symlink() {
        ("link", std::fs::read_link(path)?).hash(&mut hasher);
    } else if meta.is_dir() {
        "dir".hash(&mut hasher);
    } else {
        (std::fs::read(path)?, run_bits(&meta)).hash(&mut hasher);
    }
    Ok((meta.is_dir(), hasher.finish()))
}

/// Each entry under `dir` that is not in a store, by its path from `root`.
fn tree(root: &Path, dir: &Path, into: &mut BTreeMap<String, u64>) -> Result<(), String> {
    let unread = |e: std::io::Error| format!("cannot read {}: {e}", dir.display());
    for entry in std::fs::read_dir(dir).map_err(unread)? {
        let path = entry.map_err(unread)?.path();
        if STORES.iter().any(|store| path.ends_with(store)) {
            continue;
        }
        let shown = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        let (enter, hash) = held(&path).map_err(unread)?;
        into.insert(shown.replace('\\', "/"), hash);
        if enter {
            tree(root, &path, into)?;
        }
    }
    Ok(())
}

/// The first path the two trees hold differently, or hold on one side only.
fn first_unlike<'a>(
    old: &'a BTreeMap<String, u64>,
    new: &'a BTreeMap<String, u64>,
) -> Option<&'a String> {
    let paths = old.keys().chain(new.keys());
    paths.filter(|path| old.get(*path) != new.get(*path)).min()
}

fn quoted(text: &str) -> String {
    text.chars().take(QUOTED).collect()
}

/// One field that differs: where, both values, and the first line that is not the same.
fn difference(field: &str, at: &Value, old: &str, new: &str) -> Value {
    let same = old
        .lines()
        .zip(new.lines())
        .take_while(|(a, b)| a == b)
        .count();
    json!({
        "field": field,
        "at": at,
        "old": quoted(old),
        "new": quoted(new),
        "line": same + 1,
        "old_line": old.lines().nth(same),
        "new_line": new.lines().nth(same),
    })
}

/// The parts of one command's answer that differ. Two builds that both ran out of time differ too:
/// neither one answered.
fn unlike(old: &Answer, new: &Answer) -> Vec<&'static str> {
    let parts = [
        ("exit", old.exit != new.exit || old.exit == TIMEOUT),
        ("stdout", old.stdout != new.stdout),
        ("stderr", old.stderr != new.stderr),
    ];
    let named = parts.iter().filter(|(_, differs)| *differs);
    named.map(|(part, _)| *part).collect()
}

fn part<'a>(answer: &'a Answer, name: &str) -> &'a str {
    match name {
        "exit" => &answer.exit,
        "stdout" => &answer.stdout,
        _ => &answer.stderr,
    }
}

/// One difference for each part of a step that the two builds answered differently.
fn steps(scenario: &Scenario, old: &Ran, new: &Ran, found: &mut Vec<Value>) {
    let steps = scenario.steps.iter().zip(old.steps.iter().zip(&new.steps));
    for (n, (args, (a, b))) in steps.enumerate() {
        let at = json!({"step": n + 1, "args": args});
        let unlike = unlike(a, b).into_iter();
        found.extend(unlike.map(|name| difference(name, &at, part(a, name), part(b, name))));
    }
}

/// One difference for each part of a probe that the two builds answered differently.
fn probes(plan: &Plan, old: &Ran, new: &Ran, found: &mut Vec<Value>) {
    let probes = plan.probes.iter().zip(old.probes.iter().zip(&new.probes));
    for (args, (a, b)) in probes {
        for name in unlike(a, b) {
            let at = json!({"probe": args, "part": name});
            found.push(difference("probe", &at, part(a, name), part(b, name)));
        }
    }
}

/// Every difference between the two builds over one scenario, in the order a reader meets them.
fn differences(plan: &Plan, scenario: &Scenario, old: &Ran, new: &Ran) -> Vec<Value> {
    let mut found = Vec::new();
    steps(scenario, old, new, &mut found);
    probes(plan, old, new, &mut found);
    if let Some(path) = first_unlike(&old.tree, &new.tree) {
        found.push(json!({"field": "tree", "at": {"path": path}}));
    }
    found
}

/// `equal`, `allowed` when a person accepted each field that differs, or `different`.
fn verdict(name: &str, fields: &BTreeSet<&str>, allowed: &[Allowed]) -> &'static str {
    let accepted =
        |field: &&str| (allowed.iter()).any(|line| line.scenario == name && line.field == **field);
    match (fields.is_empty(), fields.iter().all(accepted)) {
        (true, _) => "equal",
        (false, true) => "allowed",
        (false, false) => "different",
    }
}

fn judged(plan: &Plan, scenario: &Scenario, old: &Ran, new: &Ran) -> Value {
    let found = differences(plan, scenario, old, new);
    let fields: BTreeSet<&str> = found.iter().filter_map(|d| d["field"].as_str()).collect();
    let reasons: Vec<Value> = (plan.allowed.iter())
        .filter(|line| line.scenario == scenario.name && fields.contains(line.field.as_str()))
        .map(|line| json!({"field": line.field, "reason": line.reason}))
        .collect();
    let verdict = verdict(&scenario.name, &fields, &plan.allowed);
    json!({"name": scenario.name, "verdict": verdict, "differences": found, "allowed": reasons})
}

/// One scenario on both builds at once, each in its own directories.
fn both(plan: &Plan, n: usize, scenario: &Scenario) -> Result<(Ran, Ran), String> {
    let base = plan.scratch.join(n.to_string());
    let side =
        |binary: &Path, name: &str| Side::made(plan, scenario, binary, base.join(name))?.ran();
    let (new, old) = std::thread::scope(|scope| {
        let old = scope.spawn(|| side(&plan.old, "old"));
        (side(&plan.new, "new"), old.join())
    });
    let old = old.unwrap_or_else(|_| Err("the old build's side stopped".to_string()))?;
    Ok((old, new?))
}

/// The row of one scenario: its verdict, or why it could not be compared.
fn compared(plan: &Plan, n: usize, scenario: &Scenario) -> Value {
    match both(plan, n, scenario) {
        Ok((old, new)) => judged(plan, scenario, &old, &new),
        Err(why) => json!({"name": scenario.name, "verdict": "error", "error": why}),
    }
}

/// What every scenario showed.
#[derive(Debug, Default)]
pub struct Report {
    rows: Vec<Value>,
    hits: BTreeMap<String, usize>,
}

impl Report {
    fn count(&self, verdict: &str) -> usize {
        (self.rows.iter())
            .filter(|row| row["verdict"] == verdict)
            .count()
    }

    /// `2` when a scenario could not be compared, `1` for a difference nobody accepted, else `0`.
    #[must_use]
    pub fn code(&self) -> u8 {
        match (self.count("error"), self.count("different")) {
            (0, 0) => 0,
            (0, _) => 1,
            _ => 2,
        }
    }

    #[must_use]
    pub fn render_json(&self, version: &str) -> String {
        let whole = json!({
            "chock": version,
            "scenarios": self.rows.len(),
            "equal": self.count("equal"),
            "different": self.count("different"),
            "allowed": self.count("allowed"),
            "errors": self.count("error"),
            "normalized": self.hits,
            "order": "the two builds of a scenario run at the same time; scenarios run in turn",
            "network": "proxy variables name a closed port; chock does not block sockets",
            "rows": self.rows,
        });
        serde_json::to_string_pretty(&whole).unwrap_or_default()
    }

    /// The totals, then one line for each scenario that is not equal.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "chock oracle: {} scenario(s): {} equal, {} different, {} allowed, {} error(s)\n",
            self.rows.len(),
            self.count("equal"),
            self.count("different"),
            self.count("allowed"),
            self.count("error"),
        );
        for row in self.rows.iter().filter(|row| row["verdict"] != "equal") {
            out.push_str(&line(row));
        }
        out
    }
}

/// One scenario for a person: its verdict, its name, and the first thing that explains it.
fn line(row: &Value) -> String {
    let first = &row["differences"][0];
    let why = match row["error"].as_str() {
        Some(error) => error.to_string(),
        None => format!(
            "{} {}",
            first["field"].as_str().unwrap_or_default(),
            first["at"]
        ),
    };
    format!(
        "  {:<10} {}  {why}\n",
        row["verdict"].as_str().unwrap_or_default(),
        row["name"].as_str().unwrap_or_default(),
    )
}

/// Runs each scenario of the corpus on both builds. An `Err` is a run that could not start.
pub fn run(asked: &Asked) -> Result<Report, String> {
    let plan = Plan::of(asked)?;
    // outpost: ignore[rust-path-traversal] each side works under the scratch path chock made.
    let row = |(n, scenario)| compared(&plan, n, scenario);
    let rows = plan.corpus.iter().enumerate().map(row).collect();
    // outpost: ignore[rust-path-traversal, discarded-result] a leftover of chock's own is harmless.
    let _ = std::fs::remove_dir_all(&plan.scratch);
    let hits = plan.hits.into_inner().unwrap_or_default();
    Ok(Report { rows, hits })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn rule(kind: &str, text: &str, min: usize) -> Rule {
        Rule {
            kind: kind.to_string(),
            text: text.to_string(),
            min,
            token: "<X>".to_string(),
        }
    }

    fn answer(exit: &str, stdout: &str, stderr: &str) -> Answer {
        Answer {
            exit: exit.to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn each_flag_takes_the_value_after_it() {
        let first = "--old|a|--new|b|--corpus|c|--fixture|f|--probe|p one|--probe|q";
        let then = "--normalize|n|--allow|w|--timeout|7";
        let words: Vec<&str> = first.split('|').chain(then.split('|')).collect();
        let read = asked(&words);
        let wanted = Asked {
            old: "a",
            new: "b",
            corpus: "c",
            fixture: Some("f"),
            probes: vec!["p one", "q"],
            normalize: Some("n"),
            allow: Some("w"),
            timeout: 7,
        };
        assert_eq!(read, Ok(wanted));
    }

    #[test]
    fn a_command_line_with_only_what_is_required_waits_a_minute_for_each_command() {
        let read = asked(&["--old", "a", "--new", "b", "--corpus", "c"]).unwrap();
        assert_eq!(read.timeout, 60);
        assert_eq!(
            (read.fixture, read.normalize, read.allow),
            (None, None, None)
        );
    }

    #[test]
    fn a_command_line_chock_cannot_follow_says_which_word_is_wrong() {
        let said = |args: &[&str]| asked(args).unwrap_err();
        assert_eq!(said(&["--old"]), "oracle: `--old` takes a value");
        assert_eq!(said(&["--fast", "1"]), "oracle does not take `--fast`");
        assert_eq!(
            said(&["--new", "b", "--corpus", "c"]),
            "oracle needs `--old`"
        );
        assert_eq!(
            said(&["--old", "a", "--corpus", "c"]),
            "oracle needs `--new`"
        );
        assert_eq!(
            said(&["--old", "a", "--new", "b"]),
            "oracle needs `--corpus`"
        );
        for limit in ["0", "soon"] {
            let why = said(&["--timeout", limit]);
            assert!(why.contains("seconds above zero"), "{why}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_corpus_line_that_does_not_read_is_named_by_its_number() {
        let text = "\n{\"name\":\"a\",\"steps\":[[\"x\"]]}\n\n{\"name\":\"b\",\"step\":[]}\n";
        let dir = crate::testdir::tree("oracle-lines", &[("corpus.jsonl", text)]);
        let path = dir.join("corpus.jsonl");
        let why = lines::<Scenario>(path.to_str().unwrap()).unwrap_err();
        assert!(
            why.contains("corpus.jsonl:4: unknown field `step`"),
            "{why}"
        );
        let gone = lines::<Scenario>("no/such/corpus.jsonl").unwrap_err();
        assert!(
            gone.starts_with("cannot read no/such/corpus.jsonl"),
            "{gone}"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn blank_corpus_lines_are_skipped_and_what_a_scenario_leaves_out_is_empty() {
        let text = "\n{\"name\":\"a\",\"steps\":[[\"x\",\"y\"]]}\n  \n";
        let dir = crate::testdir::tree("oracle-blank", &[("corpus.jsonl", text)]);
        let read = lines::<Scenario>(dir.join("corpus.jsonl").to_str().unwrap()).unwrap();
        let names: Vec<&str> = read.iter().map(|it| it.name.as_str()).collect();
        assert_eq!(names, ["a"]);
        let only = &read[0];
        assert_eq!(only.steps, [["x", "y"]]);
        assert!(only.stdin.is_empty() && only.env.is_empty());
        assert!(optional::<Scenario>(None).unwrap().is_empty());
    }

    #[test]
    fn a_rule_chock_cannot_apply_is_refused_with_the_reason() {
        assert_eq!(refused(&rule("text", "a", 0)), None);
        assert_eq!(refused(&rule("hex", "", 1)), None);
        assert_eq!(refused(&rule("digits", "", 3)), None);
        let why = |kind, text, min| refused(&rule(kind, text, min)).unwrap();
        assert_eq!(why("text", "", 4), "a `text` rule needs `text`");
        assert_eq!(why("hex", "a", 0), "a `hex` rule needs `min` above zero");
        assert_eq!(
            why("digits", "", 0),
            "a `digits` rule needs `min` above zero"
        );
        assert_eq!(
            why("regex", "a", 1),
            "no rule kind `regex`: text, hex or digits"
        );
    }

    #[test]
    fn a_run_as_long_as_the_rule_asks_becomes_the_token_and_a_shorter_one_stays() {
        let hex = rule("hex", "", 4);
        assert_eq!(
            applied(&hex, "id beef1 bee é cafe"),
            ("id <X> bee é <X>".to_string(), 2)
        );
        assert_eq!(applied(&hex, ""), (String::new(), 0));
        let digits = rule("digits", "", 2);
        assert_eq!(
            applied(&digits, "7 in 12ms, 345"),
            ("7 in <X>ms, <X>".to_string(), 2)
        );
        assert_eq!(applied(&digits, "abcdef"), ("abcdef".to_string(), 0));
    }

    #[test]
    fn a_text_rule_replaces_each_place_the_text_stands() {
        let text = rule("text", "/tmp/a", 0);
        assert_eq!(
            applied(&text, "in /tmp/a and /tmp/a/b"),
            ("in <X> and <X>/b".to_string(), 2)
        );
        assert_eq!(applied(&text, "none"), ("none".to_string(), 0));
    }

    #[test]
    fn the_parts_of_an_answer_that_differ_are_named_and_a_timeout_never_matches() {
        let base = answer("0", "out", "err");
        assert!(unlike(&base, &base).is_empty());
        assert_eq!(unlike(&base, &answer("1", "out", "err")), ["exit"]);
        assert_eq!(unlike(&base, &answer("0", "other", "err")), ["stdout"]);
        assert_eq!(unlike(&base, &answer("0", "out", "other")), ["stderr"]);
        let slow = answer(TIMEOUT, "", "");
        assert_eq!(unlike(&slow, &slow), ["exit"]);
        assert_eq!(part(&base, "exit"), "0");
        assert_eq!(part(&base, "stdout"), "out");
        assert_eq!(part(&base, "stderr"), "err");
    }

    #[test]
    fn a_difference_quotes_both_values_and_the_first_line_that_is_not_the_same() {
        let at = json!({"step": 1});
        let row = difference("stdout", &at, "a\nb\nc\n", "a\nx\nc\n");
        assert_eq!(row["field"], "stdout");
        assert_eq!(row["at"], at);
        assert_eq!(row["line"], 2);
        assert_eq!(row["old_line"], "b");
        assert_eq!(row["new_line"], "x");
        assert_eq!(row["old"], "a\nb\nc\n");
        let longer = difference("stdout", &at, "a\n", "a\nmore\n");
        assert_eq!(longer["line"], 2);
        assert_eq!(longer["old_line"], Value::Null);
        assert_eq!(longer["new_line"], "more");
        let long = "é".repeat(QUOTED + 5);
        assert_eq!(quoted(&long), "é".repeat(QUOTED));
    }

    #[test]
    fn the_first_path_two_trees_hold_differently_is_named_whichever_side_has_it() {
        let tree = |files: &[(&str, u64)]| -> BTreeMap<String, u64> {
            files
                .iter()
                .map(|(path, hash)| ((*path).to_string(), *hash))
                .collect()
        };
        let old = tree(&[("a", 1), ("m", 2), ("z", 3)]);
        assert_eq!(first_unlike(&old, &old), None);
        let changed = tree(&[("a", 1), ("m", 9), ("z", 8)]);
        assert_eq!(first_unlike(&old, &changed).map(String::as_str), Some("m"));
        let added = tree(&[("a", 1), ("b", 5), ("m", 2), ("z", 3)]);
        assert_eq!(first_unlike(&old, &added).map(String::as_str), Some("b"));
        assert_eq!(first_unlike(&added, &old).map(String::as_str), Some("b"));
    }

    #[test]
    fn a_scenario_is_allowed_only_when_a_person_accepted_each_field_that_differs() {
        let allowed = [Allowed {
            scenario: "log".to_string(),
            field: "stderr".to_string(),
            reason: "the new build drops a warning".to_string(),
        }];
        let fields = |names: &[&'static str]| names.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(verdict("log", &fields(&[]), &allowed), "equal");
        assert_eq!(verdict("log", &fields(&["stderr"]), &allowed), "allowed");
        assert_eq!(
            verdict("log", &fields(&["stderr", "exit"]), &allowed),
            "different"
        );
        assert_eq!(
            verdict("status", &fields(&["stderr"]), &allowed),
            "different"
        );
        assert_eq!(verdict("log", &fields(&["stdout"]), &[]), "different");
    }

    fn report(verdicts: &[&str]) -> Report {
        let rows = (verdicts.iter())
            .map(|verdict| json!({"name": "n", "verdict": verdict, "differences": []}))
            .collect();
        Report {
            rows,
            hits: BTreeMap::from([("<DIR>".to_string(), 3)]),
        }
    }

    #[test]
    fn the_exit_code_is_two_for_an_error_one_for_a_difference_and_zero_otherwise() {
        assert_eq!(report(&[]).code(), 0);
        assert_eq!(report(&["equal", "allowed"]).code(), 0);
        assert_eq!(report(&["equal", "different"]).code(), 1);
        assert_eq!(report(&["different", "error"]).code(), 2);
        assert_eq!(report(&["error"]).code(), 2);
    }

    #[test]
    fn the_json_report_counts_each_verdict_and_each_token() {
        let shown =
            report(&["equal", "equal", "different", "allowed", "error"]).render_json("9.9.9");
        let read: Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(read["chock"], "9.9.9");
        assert_eq!(read["scenarios"], 5);
        assert_eq!(read["equal"], 2);
        assert_eq!(read["different"], 1);
        assert_eq!(read["allowed"], 1);
        assert_eq!(read["errors"], 1);
        assert_eq!(read["normalized"]["<DIR>"], 3);
        assert_eq!(read["rows"][2]["verdict"], "different");
        assert!(
            read["network"]
                .as_str()
                .unwrap()
                .contains("does not block sockets")
        );
        assert!(read["order"].as_str().unwrap().contains("at the same time"));
    }

    #[test]
    fn the_text_report_gives_the_totals_and_a_line_for_each_scenario_that_is_not_equal() {
        let mut held = report(&["equal"]);
        held.rows.push(json!({
            "name": "log", "verdict": "different",
            "differences": [{"field": "stdout", "at": {"step": 2}}],
        }));
        held.rows
            .push(json!({"name": "push", "verdict": "error", "error": "cannot start"}));
        assert_eq!(
            held.render(),
            "chock oracle: 3 scenario(s): 1 equal, 1 different, 0 allowed, 1 error(s)\n  \
             different  log  stdout {\"step\":2}\n  error      push  cannot start\n"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_is_read_by_path_and_content_and_a_store_is_left_out() {
        let files = [
            ("a.txt", "one"),
            ("sub/b.txt", "two"),
            (".git/HEAD", "x"),
            ("sub/.outpost/db", "y"),
        ];
        let dir = crate::testdir::tree("oracle-tree", &files);
        let read = |root: &Path| {
            let mut into = BTreeMap::new();
            tree(root, root, &mut into).unwrap();
            into
        };
        let first = read(&dir);
        let paths: Vec<&str> = first.keys().map(String::as_str).collect();
        assert_eq!(paths, ["a.txt", "sub", "sub/b.txt"]);
        assert_eq!(read(&dir), first);
        std::fs::write(dir.join("sub/b.txt"), "changed").unwrap();
        let second = read(&dir);
        assert_eq!(
            first_unlike(&first, &second).map(String::as_str),
            Some("sub/b.txt")
        );
        assert_eq!(first["a.txt"], second["a.txt"]);
        let mut none = BTreeMap::new();
        let why = tree(&dir, &dir.join("absent"), &mut none).unwrap_err();
        assert!(why.starts_with("cannot read "), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_may_run_and_a_link_are_each_told_from_a_plain_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testdir::tree("oracle-kinds", &[("tool", "same"), ("plain", "same")]);
        std::os::unix::fs::symlink("plain", dir.join("link")).unwrap();
        let (plain, link) = (
            held(&dir.join("plain")).unwrap(),
            held(&dir.join("link")).unwrap(),
        );
        assert_eq!(held(&dir.join("tool")).unwrap(), plain);
        std::fs::set_permissions(dir.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(held(&dir.join("tool")).unwrap(), plain);
        let bits = |name: &str| run_bits(&std::fs::metadata(dir.join(name)).unwrap());
        assert_eq!((bits("tool"), bits("plain")), (0o111, 0));
        assert_ne!(link, plain);
        assert!(!link.0 && !plain.0);
        assert!(held(&dir).unwrap().0);
        assert!(held(&dir.join("absent")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_fixture_is_copied_with_its_directories_and_its_links_stay_links() {
        let from = crate::testdir::tree(
            "oracle-from",
            &[("a.txt", "one"), ("sub/deep/b.txt", "two")],
        );
        std::os::unix::fs::symlink("a.txt", from.join("link")).unwrap();
        let to = crate::testdir::make("oracle-to");
        copied(&from, &to.join("work")).unwrap();
        let read = |path: &str| std::fs::read_to_string(to.join("work").join(path)).unwrap();
        assert_eq!(read("a.txt"), "one");
        assert_eq!(read("sub/deep/b.txt"), "two");
        let link = std::fs::read_link(to.join("work/link")).unwrap();
        assert_eq!(link, Path::new("a.txt"));
        let why = copied(&from.join("absent"), &to.join("other")).unwrap_err();
        assert!(why.starts_with("cannot copy "), "{why}");
    }

    /// One corpus line that runs `script` in a shell.
    #[cfg(unix)]
    fn scenario(name: &str, script: &str) -> String {
        format!("{}\n", json!({"name": name, "steps": [["-c", script]]}))
    }

    /// A run with `sh` as the old build and `bash` as the new one. A script tells the two apart by
    /// `$0`, so no test has to write a program of its own.
    #[cfg(unix)]
    fn compared_shells(name: &str, corpus: &str, more: &[(&str, &str)]) -> Report {
        let dir = crate::testdir::tree(name, &[("corpus.jsonl", corpus)]);
        let corpus = dir.join("corpus.jsonl");
        let mut args = vec!["--old", "/bin/sh", "--new", "/bin/bash", "--corpus"];
        args.push(corpus.to_str().unwrap());
        args.extend(more.iter().flat_map(|(flag, value)| [*flag, *value]));
        run(&asked(&args).unwrap()).unwrap()
    }

    /// Prints `$0`-dependent text: what the new build says, and what the old one says.
    #[cfg(unix)]
    fn each(new: &str, old: &str) -> String {
        format!("case $0 in *bash) {new};; *) {old};; esac")
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn two_builds_that_answer_the_same_are_equal_though_each_ran_in_its_own_directory() {
        let script = "pwd; pwd >&2; echo kept > kept.txt; mkdir .git; echo $0 > .git/HEAD";
        let report = compared_shells("oracle-equal", &scenario("same", script), &[]);
        assert_eq!(report.rows[0]["verdict"], "equal", "{:?}", report.rows);
        assert_eq!(report.code(), 0);
        assert_eq!(report.hits["<DIR>"], 4);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn each_build_gets_the_same_fixed_environment_and_nothing_from_the_caller() {
        let new = "echo \"$TZ/$LC_ALL/$NO_COLOR/${CARGO_MANIFEST_DIR:-none}/${HOME##*/}/$GIT_AUTHOR_NAME\"";
        let script = each(new, "echo 'UTC/C/1/none/home/chock oracle'");
        let report = compared_shells("oracle-env", &scenario("env", &script), &[]);
        assert_eq!(report.rows[0]["verdict"], "equal", "{:?}", report.rows);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_scenario_gives_its_own_stdin_and_environment_to_every_step() {
        let read = each(
            "read line; echo \"$line in $TZ\"",
            "echo 'typed in Asia/Kolkata'",
        );
        let line = json!({
            "name": "fed", "steps": [["-c", "echo first"], ["-c", read]],
            "stdin": "typed\n", "env": {"TZ": "Asia/Kolkata"},
        });
        let report = compared_shells("oracle-stdin", &format!("{line}\n"), &[]);
        assert_eq!(report.rows[0]["verdict"], "equal", "{:?}", report.rows);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_difference_names_its_step_its_stream_and_both_values() {
        let script = each("echo same; echo new >&2; exit 4", "echo same; echo old >&2");
        let corpus = scenario("ok", "echo ok") + &scenario("unlike", &script);
        let report = compared_shells("oracle-unlike", &corpus, &[]);
        assert_eq!(report.rows[0]["verdict"], "equal");
        let row = &report.rows[1];
        assert_eq!(row["verdict"], "different");
        let fields: Vec<&str> = (row["differences"].as_array().unwrap().iter())
            .filter_map(|found| found["field"].as_str())
            .collect();
        assert_eq!(fields, ["exit", "stderr"]);
        assert_eq!(row["differences"][0]["old"], "0");
        assert_eq!(row["differences"][0]["new"], "4");
        assert_eq!(row["differences"][1]["at"]["step"], 1);
        assert_eq!(row["differences"][1]["old_line"], "old");
        assert_eq!(row["differences"][1]["new_line"], "new");
        assert_eq!(report.code(), 1);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn an_accepted_difference_stays_in_the_report_and_no_longer_fails_the_run() {
        let script = each("echo new >&2", "echo old >&2");
        let allow = json!({"scenario": "warns", "field": "stderr", "reason": "a warning went"});
        let dir = crate::testdir::tree("oracle-allow", &[("allow.jsonl", &format!("{allow}\n"))]);
        let allow = dir.join("allow.jsonl");
        let more = [("--allow", allow.to_str().unwrap())];
        let report = compared_shells("oracle-allowed", &scenario("warns", &script), &more);
        let row = &report.rows[0];
        assert_eq!(row["verdict"], "allowed", "{row}");
        assert_eq!(row["differences"][0]["field"], "stderr");
        assert_eq!(row["allowed"][0]["reason"], "a warning went");
        assert_eq!(report.code(), 0);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn text_a_rule_covers_is_equal_and_the_report_counts_what_the_rule_replaced() {
        let script = each("echo id 0123abcd9", "echo id fedcba987");
        let rules = json!({"kind": "hex", "min": 8, "token": "<ID>"});
        let dir = crate::testdir::tree("oracle-rules", &[("rules.jsonl", &format!("{rules}\n"))]);
        let rules = dir.join("rules.jsonl");
        let more = [("--normalize", rules.to_str().unwrap())];
        let report = compared_shells("oracle-normal", &scenario("ids", &script), &more);
        assert_eq!(report.rows[0]["verdict"], "equal", "{:?}", report.rows);
        assert_eq!(report.hits["<ID>"], 2);
        let unruled = compared_shells("oracle-unruled", &scenario("ids", &script), &[]);
        assert_eq!(unruled.rows[0]["verdict"], "different");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_file_the_builds_wrote_differently_is_named_and_the_fixture_itself_is_not_touched() {
        let fixture = crate::testdir::tree("oracle-fixture", &[("data/seed.txt", "seed\n")]);
        let script = "cat data/seed.txt; echo $0 >> data/seed.txt";
        let more = [("--fixture", fixture.to_str().unwrap())];
        let report = compared_shells("oracle-files", &scenario("writes", script), &more);
        let row = &report.rows[0];
        assert_eq!(row["verdict"], "different", "{row}");
        let found = &row["differences"][0];
        assert!(row["differences"][1].is_null(), "{row}");
        assert_eq!(found["field"], "tree");
        assert_eq!(found["at"]["path"], "data/seed.txt");
        let seed = std::fs::read_to_string(fixture.join("data/seed.txt")).unwrap();
        assert_eq!(seed, "seed\n");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_probe_runs_after_the_steps_and_what_it_shows_is_compared() {
        let more = [("--probe", "-c pwd"), ("--probe", "-c echo$IFS$0")];
        let report = compared_shells("oracle-probe", &scenario("quiet", "true"), &more);
        let row = &report.rows[0];
        assert_eq!(row["verdict"], "different", "{row}");
        let found = &row["differences"][0];
        assert!(row["differences"][1].is_null(), "{row}");
        assert_eq!(found["field"], "probe");
        assert_eq!(found["at"]["part"], "stdout");
        assert_eq!(found["at"]["probe"][1], "echo$IFS$0");
        assert_eq!(found["old"], "/bin/sh\n");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn two_builds_that_both_run_out_of_time_are_a_difference_and_never_equal() {
        let started = std::time::Instant::now();
        let more = [("--timeout", "1")];
        let report = compared_shells("oracle-slow", &scenario("hangs", "sleep 600"), &more);
        let row = &report.rows[0];
        assert_eq!(row["verdict"], "different", "{row}");
        assert_eq!(row["differences"][0]["field"], "exit");
        assert_eq!(row["differences"][0]["old"], TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_step_that_writes_more_than_chock_keeps_is_an_error_and_is_not_compared() {
        let flood = scenario("flood", "head -c 17000000 /dev/zero");
        let report = compared_shells("oracle-flood", &flood, &[]);
        let row = &report.rows[0];
        assert_eq!(row["verdict"], "error");
        let why = "`-c head -c 17000000 /dev/zero` wrote more than chock keeps of one stream";
        assert_eq!(row["error"], why);
        assert_eq!(report.code(), 2);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_build_that_does_not_start_is_an_error_and_a_run_with_an_error_exits_two() {
        let dir =
            crate::testdir::tree("oracle-broken", &[("corpus.jsonl", &scenario("s", "true"))]);
        let corpus = dir.join("corpus.jsonl");
        let corpus = corpus.to_str().unwrap();
        let not_a_program = asked(&["--old", "/bin/sh", "--new", corpus, "--corpus", corpus]);
        let report = run(&not_a_program.unwrap()).unwrap();
        assert_eq!(report.rows[0]["verdict"], "error", "{:?}", report.rows);
        assert!(
            report.rows[0]["error"]
                .as_str()
                .unwrap()
                .contains("corpus.jsonl")
        );
        assert_eq!(report.code(), 2);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_whose_inputs_are_wrong_does_not_start() {
        let rules = "{\"kind\":\"regex\",\"token\":\"<T>\"}\n";
        let files = [("corpus.jsonl", ""), ("rules.jsonl", rules), ("build", "")];
        let dir = crate::testdir::tree("oracle-inputs", &files);
        let path = |name: &str| dir.join(name).to_str().unwrap().to_string();
        let (build, corpus, rules) = (path("build"), path("corpus.jsonl"), path("rules.jsonl"));
        let why = |args: &[&str]| run(&asked(args).unwrap()).map(|_| ()).unwrap_err();
        let base = ["--old", &build, "--new", &build, "--corpus", &corpus];
        let with = |more: &[&str]| why(&[&base[..], more].concat());
        assert!(
            with(&["--normalize", &rules]).ends_with("no rule kind `regex`: text, hex or digits")
        );
        assert!(with(&["--fixture", &build]).ends_with("is not a directory"));
        assert!(with(&["--allow", "no/such/file"]).starts_with("cannot read no/such/file"));
        let absent = why(&[
            "--old",
            "no/such/build",
            "--new",
            &build,
            "--corpus",
            &corpus,
        ]);
        assert_eq!(absent, "no/such/build is not a file");
        assert!(why(&base).ends_with("corpus.jsonl holds no scenario"));
    }
}
