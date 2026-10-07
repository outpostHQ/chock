//! `chock edited`: what the commit would refuse in one file, checked on each write without a build.

use std::collections::BTreeMap;

use crate::gates::metrics::{complexity, nesting, prodlines};
use crate::gates::source;
use crate::run::report::Finding;

/// Everything in this file the commit would refuse. A Rust file that does not parse is one finding.
#[must_use]
pub fn faults(path: &str, src: &str, rules: &Rules) -> Vec<Finding> {
    let mut found = rules.past("slop", &crate::slop::skipped, blocks(path, src));
    let phrases = crate::gates::text::phrases::found(path, src, &rules.forbidden);
    let skipped = |part: &str| crate::project::SKIPPED.contains(&part);
    found.extend(rules.past("phrases", &skipped, phrases));
    // The remaining rules read a Rust syntax tree.
    if !path.ends_with(RUST) {
        return found;
    }
    // Judged as the `source` gate does: shipped unless `not_shipped`, unread if never compiled.
    match source::judged(path, src, !rules.unshipped) {
        Ok(suppressions) => found.extend(rules.past("source", &|_| rules.uncompiled, suppressions)),
        Err(why) => return vec![Finding::at(path, &format!("does not parse: {why}"))],
    }
    found.extend(too_complicated(path, src, rules));
    found.extend(too_deep(path, src, rules));
    found
}

/// What the commit accepts in one file: the gates switched on, the baseline's levels and the
/// phrases that apply. The default is no project, where every rule applies.
#[derive(Debug, Default)]
pub struct Rules {
    pub forbidden: Vec<crate::project::config::Forbidden>,
    shown: String,
    config: Option<crate::project::config::Config>,
    baseline: Option<crate::run::baseline::Baseline>,
    unshipped: bool,
    uncompiled: bool,
}

impl Rules {
    /// Whether `gate` is on and walks into this file; `skips` is the gate's own directory filter.
    fn speaks(&self, gate: &str, skips: &dyn Fn(&str) -> bool) -> bool {
        self.config.as_ref().is_none_or(|config| config.is_on(gate))
            && !self.shown.split('/').any(skips)
    }

    /// An edited file is always one the change touches, so `clean_when_touched` holds it to zero.
    fn held(&self, gate: &str, key: &str) -> u64 {
        let listed = |config: &crate::project::config::Config| {
            config
                .clean_when_touched
                .iter()
                .flatten()
                .any(|name| name == gate)
        };
        if self.config.as_ref().is_some_and(listed) {
            return 0;
        }
        self.baseline
            .as_ref()
            .and_then(|recorded| recorded.gate(gate).get(key))
            .unwrap_or(0)
    }

    /// A per-function score past the gate's limit and past what the baseline holds for it.
    fn over(
        &self,
        gate: &str,
        skips: &dyn Fn(&str) -> bool,
        name: &str,
        score: u64,
        limit: u64,
    ) -> bool {
        score > limit
            && score > self.held(gate, &format!("{}#{name}", self.shown))
            && self.speaks(gate, skips)
    }

    /// The findings of each key whose count exceeds what the baseline holds. Such a key reports all
    /// of them, since the new one cannot be told apart.
    fn past(&self, gate: &str, skips: &dyn Fn(&str) -> bool, found: Vec<Finding>) -> Vec<Finding> {
        if !self.speaks(gate, skips) {
            return Vec::new();
        }
        let key = |finding: &Finding| match &finding.item {
            Some(item) => format!("{}#{item}", self.shown),
            None => self.shown.clone(),
        };
        let mut counts = BTreeMap::<String, u64>::new();
        for finding in &found {
            *counts.entry(key(finding)).or_default() += 1;
        }
        found
            .into_iter()
            .filter(|finding| {
                let at = key(finding);
                counts.get(&at).copied().unwrap_or(0) > self.held(gate, &at)
            })
            .collect()
    }
}

/// The project an edited file belongs to: the nearest crate above it set up for chock, else its
/// own nearest `Cargo.toml`.
#[must_use]
pub fn project_of(path: &str) -> Option<std::path::PathBuf> {
    project_with(&absolute(path), &|path| path.is_file())
}

/// Whether the project is set up for chock, so a commit there runs its gates.
#[must_use]
pub fn adopted(root: &std::path::Path) -> bool {
    root.join(crate::project::config::FILE).is_file()
}

/// The editor's path, made absolute against chock's working directory.
fn absolute(path: &str) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| std::path::PathBuf::from(path))
}

/// `project_of`, with the existence test injected so it can be tested inside another project.
fn project_with(
    file: &std::path::Path,
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    let set_up = |manifest: &std::path::Path| {
        exists(manifest)
            && manifest
                .parent()
                .is_some_and(|dir| exists(&dir.join(crate::project::config::FILE)))
    };
    let dir = file.parent()?;
    crate::project::find_with(dir, &set_up).or_else(|| crate::project::find_with(dir, exists))
}

/// The rules for one edited file. Best effort: an unreadable config or baseline is ignored here,
/// and the commit's gates refuse it.
#[must_use]
pub fn rules_for(root: Option<&std::path::Path>, path: &str) -> Rules {
    let Some(root) = root else {
        return Rules::default();
    };
    let config = crate::project::document::read::<crate::project::config::Config>(root)
        .ok()
        .flatten();
    let shown = crate::project::relative(root, &absolute(path));
    let listed = config
        .as_ref()
        .and_then(|set| set.forbidden.clone())
        .unwrap_or_default();
    let not_shipped = config
        .as_ref()
        .and_then(|set| set.not_shipped.clone())
        .unwrap_or_default();
    Rules {
        forbidden: crate::gates::text::phrases::applying(&listed, &shown),
        baseline: crate::project::document::read::<crate::run::baseline::Baseline>(root)
            .ok()
            .flatten(),
        unshipped: !crate::project::ships(&shown, &not_shipped),
        uncompiled: source::unread(root, &shown),
        config,
        shown,
    }
}

/// Comment blocks as `slop` counts them, which passes over a file marked exempt.
fn blocks(path: &str, src: &str) -> Vec<Finding> {
    if crate::slop::is_exempt(src) {
        return Vec::new();
    }
    comments(path, src)
}

/// The files `nesting` never reads: test directories, fixtures, and a `tests.rs` of its own.
fn not_production(part: &str) -> bool {
    prodlines::skip_dir(part) || prodlines::is_test_file(part)
}

/// Findings as the editor is shown them, one to a line.
#[must_use]
pub fn told(found: &[Finding]) -> String {
    found
        .iter()
        .map(|finding| format!("  {}\n", finding.render()))
        .collect()
}

/// The findings as the one report `--json` prints; `edited` names the hook, not a registry gate.
#[must_use]
pub fn report(found: &[Finding]) -> crate::run::report::GateReport {
    let verdict = if found.is_empty() {
        crate::run::report::Verdict::Pass
    } else {
        crate::run::report::Verdict::Tripped
    };
    let mut report = crate::run::report::GateReport::new("edited", verdict, "chock edited <path>");
    report.findings = found.to_vec();
    report
}

const RUST: &str = ".rs";

/// Comment blocks over the limit, in any language chock knows markers for.
fn comments(path: &str, src: &str) -> Vec<Finding> {
    let Some(marks) = crate::slop::markers_for(path) else {
        return Vec::new();
    };
    crate::slop::over_length_blocks(src, marks)
        .into_iter()
        .map(|(line, length)| {
            Finding::at(
                path,
                &format!(
                    "comment block of {length} lines, over {}",
                    crate::slop::MAX_BLOCK
                ),
            )
            .line(line_of(line))
        })
        .collect()
}

fn too_complicated(path: &str, src: &str, rules: &Rules) -> Vec<Finding> {
    let limit = u64::from(complexity::HARD_TO_FOLLOW);
    complexity::functions(src, path).map_or_else(
        |_| Vec::new(),
        |found| {
            found
                .into_iter()
                .filter(|one| {
                    let score = u64::from(one.score);
                    rules.over("complexity", &complexity::skipped, &one.name, score, limit)
                })
                .map(|one| {
                    Finding::at(
                        path,
                        &format!(
                            "{}: {} cognitive, over {}",
                            one.name,
                            one.score,
                            complexity::HARD_TO_FOLLOW
                        ),
                    )
                    .line(one.line)
                })
                .collect()
        },
    )
}

fn too_deep(path: &str, src: &str, rules: &Rules) -> Vec<Finding> {
    nesting::depths(src).map_or_else(
        |_| Vec::new(),
        |found| {
            found
                .into_iter()
                .filter(|(name, depth)| {
                    let past = u64::from(depth.saturating_sub(nesting::SHALLOW));
                    rules.over("nesting", &not_production, name, past, 0)
                })
                .map(|(name, depth)| {
                    Finding::at(
                        path,
                        &format!("{name}: nested {depth} deep, over {}", nesting::SHALLOW),
                    )
                })
                .collect()
        },
    )
}

/// Where each hook firing is recorded, so `doctor` can tell a silent hook from a clean tree.
pub const HEARTBEAT: &str = ".chock/last-edit";

/// Records a firing, best effort. The file holds the time, since rewriting an empty file leaves its
/// mtime alone.
pub fn note_firing(root: &std::path::Path) {
    let path = root.join(HEARTBEAT);
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return;
    }
    let _ = std::fs::write(&path, firing_at(std::time::SystemTime::now()));
}

/// The firing time, as seconds since the epoch.
fn firing_at(now: std::time::SystemTime) -> String {
    let since = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|gone| gone.as_secs())
        .unwrap_or_default();
    format!("{since}\n")
}

/// How long since the hook last answered, or `None` if it never has.
#[must_use]
pub fn since_firing(root: &std::path::Path) -> Option<std::time::Duration> {
    let at = std::fs::metadata(root.join(HEARTBEAT))
        .and_then(|about| about.modified())
        .ok()?;
    at.elapsed().ok()
}

/// The file an editor hook says was written, from the tool's JSON or a bare path. `None` for a file
/// chock has no rules for; `Err` if the hook named no file.
pub fn edited_path(stdin: &str) -> Result<Option<String>, String> {
    // A hook wired wrongly would otherwise pass every edit.
    let path = named_path(stdin).ok_or_else(|| misconfigured(stdin))?;
    Ok(crate::slop::markers_for(&path).is_some().then_some(path))
}

/// The path in an editor's tool JSON, or a bare path. A bare path holds no whitespace, so a line of
/// prose is not read as one.
fn named_path(stdin: &str) -> Option<String> {
    let text = stdin.trim();
    if text.starts_with('{') {
        return serde_json::from_str::<Call>(text)
            .ok()
            .and_then(|call| call.tool_input.file_path);
    }
    (!text.is_empty() && !text.contains(char::is_whitespace)).then(|| text.to_string())
}

fn misconfigured(stdin: &str) -> String {
    let said = stdin.trim().lines().next().unwrap_or("nothing");
    format!(
        "the hook named no file to check. chock reads an editor's tool JSON with a \
         `tool_input.file_path`, or a bare path, and was given: {said}"
    )
}

/// The tool call a host sends, in snake_case (Claude Code) or camelCase (Copilot CLI).
#[derive(serde::Deserialize)]
struct Call {
    #[serde(default, alias = "toolInput")]
    tool_input: Input,
}

#[derive(serde::Deserialize, Default)]
struct Input {
    #[serde(default, alias = "filePath")]
    file_path: Option<String>,
}

/// For a passing gate that keys whole files, the lines behind each count the baseline holds, so
/// `explain` can name them.
#[must_use]
pub fn behind_held(
    root: &std::path::Path,
    report: &crate::run::report::GateReport,
) -> Vec<Finding> {
    let gate = report.gate.as_str();
    if !report.findings.is_empty() {
        return Vec::new();
    }
    let Ok(Some(baseline)) = crate::project::document::read::<crate::run::baseline::Baseline>(root)
    else {
        return Vec::new();
    };
    let exists = |cited: &str| crate::gates::text::citations::anywhere(root, cited);
    baseline
        .gate(gate)
        .0
        .keys()
        .filter_map(|key| Some((key, std::fs::read_to_string(root.join(key)).ok()?)))
        .flat_map(|(key, src)| occurrences(gate, key, &src, &exists))
        .collect()
}

/// Each occurrence a file-keyed gate counts in one file, ignoring the baseline.
fn occurrences(gate: &str, path: &str, src: &str, exists: &dyn Fn(&str) -> bool) -> Vec<Finding> {
    match gate {
        "slop" => blocks(path, src),
        "citations" => crate::gates::text::citations::missing(src, exists)
            .into_iter()
            .map(|(line, cited)| {
                Finding::at(
                    path,
                    &format!("names `{cited}`, which is not in the repository"),
                )
                .line(line_of(line))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A counted line number as a finding's `u32`, saturating.
fn line_of(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn rendered(path: &str, src: &str) -> Vec<String> {
        faults(path, src, &Rules::default())
            .iter()
            .map(Finding::render)
            .collect()
    }

    const HELD: &str = "/// See `docs/gone.md` and `src/here.rs`.\nfn f() {}\n// a\n// b\n// c\n";

    #[test]
    fn a_count_held_for_a_whole_file_is_traced_to_each_of_its_lines() {
        let exists = |cited: &str| cited == "src/here.rs";
        let traced = |gate: &str| -> Vec<String> {
            occurrences(gate, "src/a.rs", HELD, &exists)
                .iter()
                .map(Finding::render)
                .collect()
        };
        assert_eq!(
            traced("citations"),
            ["src/a.rs:1: names `docs/gone.md`, which is not in the repository"]
        );
        assert_eq!(
            traced("slop"),
            ["src/a.rs:3: comment block of 3 lines, over 2"]
        );
        assert_eq!(traced("complexity"), Vec::<String>::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn explain_reads_the_held_files_from_the_baseline() {
        let dir = crate::testdir::make("edited-behind-held");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        std::fs::write(dir.join("src/a.rs"), HELD).unwrap();
        let held = holding(&[("slop", "src/a.rs", 1), ("slop", "src/gone.rs", 1)]);
        std::fs::write(dir.join(crate::run::baseline::FILE), held.render()).unwrap();
        let mut passed = crate::run::report::GateReport::new(
            "slop",
            crate::run::report::Verdict::Pass,
            "chock run slop",
        );
        let traced: Vec<String> = behind_held(&dir, &passed)
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(traced, ["src/a.rs:3: comment block of 3 lines, over 2"]);
        assert_eq!(behind_held(&dir.join("nowhere"), &passed), Vec::new());
        passed.findings = vec![Finding::at("src/a.rs", "tripped")];
        assert_eq!(
            behind_held(&dir, &passed),
            Vec::new(),
            "its own findings say where"
        );
    }

    fn holding(levels: &[(&str, &str, u64)]) -> crate::run::baseline::Baseline {
        let mut recorded = crate::run::baseline::Baseline::empty("0.1.0");
        for (gate, key, level) in levels {
            let mut series = recorded.gate(gate);
            series.set(key, *level);
            recorded.set(gate, series);
        }
        recorded
    }

    fn at(shown: &str, gates: &[&str], baseline: crate::run::baseline::Baseline) -> Rules {
        Rules {
            shown: shown.to_string(),
            config: Some(crate::project::config::Config::of(gates.iter().copied())),
            baseline: Some(baseline),
            ..Rules::default()
        }
    }

    const NOWHERE: fn(&str) -> bool = |_| false;

    #[test]
    fn a_score_is_told_only_past_what_the_baseline_holds_for_that_function() {
        let rules = at(
            "src/lib.rs",
            &["complexity"],
            holding(&[("complexity", "src/lib.rs#tangled", 19)]),
        );
        let told = |name: &str, score: u64| rules.over("complexity", &NOWHERE, name, score, 15);
        assert_eq!(
            [
                told("tangled", 19),
                told("tangled", 20),
                told("fresh", 15),
                told("fresh", 16)
            ],
            [false, true, false, true]
        );
        assert!(
            !rules.over("nesting", &NOWHERE, "fresh", 9, 0),
            "nesting is off"
        );
        assert!(
            Rules::default().over("nesting", &NOWHERE, "fresh", 1, 0),
            "no project"
        );
    }

    #[test]
    fn a_gate_is_silent_about_a_file_its_own_walk_never_reaches() {
        let told = |shown: &str, gate: &str, skips: &dyn Fn(&str) -> bool| {
            at(shown, &[gate], crate::run::baseline::Baseline::default())
                .over(gate, skips, "f", 99, 0)
        };
        assert_eq!(
            [
                told("tests/cli.rs", "complexity", &complexity::skipped),
                told("benches/b.rs", "complexity", &complexity::skipped),
                told("src/lib.rs", "complexity", &complexity::skipped),
                told("tests/cli.rs", "nesting", &not_production),
                told("src/tests.rs", "nesting", &not_production),
                told("src/lib.rs", "nesting", &not_production),
            ],
            [false, false, true, false, false, true]
        );
    }

    #[test]
    fn a_counted_key_reports_all_of_its_own_once_past_what_is_held() {
        let rules = at(
            "src/lib.rs",
            &["slop", "phrases"],
            holding(&[("slop", "src/lib.rs", 1), ("phrases", "src/lib.rs#a", 1)]),
        );
        let block = |line: u32| Finding::at("src/lib.rs", "comment block").line(line);
        let phrase = |text: &str| Finding::at("src/lib.rs", "no").item(text);
        assert_eq!(rules.past("slop", &NOWHERE, vec![block(1)]), Vec::new());
        assert_eq!(
            rules.past("slop", &NOWHERE, vec![block(1), block(9)]),
            [block(1), block(9)]
        );
        assert_eq!(
            rules.past("phrases", &NOWHERE, vec![phrase("a"), phrase("b")]),
            [phrase("b")]
        );
        assert_eq!(
            rules.past("phrases", &NOWHERE, vec![phrase("a"), phrase("a")]),
            [phrase("a"), phrase("a")]
        );
        assert_eq!(
            rules.past("source", &NOWHERE, vec![phrase("a")]),
            Vec::new()
        );
        assert_eq!(
            rules.past("slop", &|part| part == "src", vec![block(1), block(9)]),
            Vec::new()
        );
    }

    #[test]
    fn a_gate_held_clean_where_touched_reports_what_the_baseline_holds() {
        let mut rules = at(
            "src/lib.rs",
            &["slop", "phrases"],
            holding(&[("slop", "src/lib.rs", 1), ("phrases", "src/lib.rs#a", 1)]),
        );
        rules.config.as_mut().unwrap().clean_when_touched = Some(vec!["slop".to_string()]);
        let block = Finding::at("src/lib.rs", "comment block").line(1);
        let phrase = Finding::at("src/lib.rs", "no").item("a");
        assert_eq!(rules.past("slop", &NOWHERE, vec![block.clone()]), [block]);
        assert_eq!(rules.past("phrases", &NOWHERE, vec![phrase]), Vec::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_editor_hears_only_what_the_commit_would_refuse() {
        let dir = crate::testdir::make("edited-held");
        let file = dir.join("src/a.rs").to_string_lossy().into_owned();
        let src = "fn f() {\n if a {\n  if b {\n   if c {\n    if d {\n     g();\n}}}}}\n";
        configured(&dir, &["nesting"], |_| ());
        let told = || -> Vec<String> {
            let rules = rules_for(Some(&dir), &file);
            faults(&file, src, &rules)
                .iter()
                .map(Finding::render)
                .collect()
        };
        assert_eq!(
            told(),
            [format!("{file}: f: nested 5 deep, over 4")],
            "nothing is held yet"
        );
        let held = holding(&[("nesting", "src/a.rs#f", 1)]);
        std::fs::write(dir.join(crate::run::baseline::FILE), held.render()).unwrap();
        assert_eq!(
            told(),
            Vec::<String>::new(),
            "the baseline holds this depth"
        );
        configured(&dir, &["slop"], |_| ());
        std::fs::remove_file(dir.join(crate::run::baseline::FILE)).unwrap();
        assert_eq!(told(), Vec::<String>::new(), "nesting is switched off");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_source_rule_reads_a_file_as_its_gate_does() {
        let dir = crate::testdir::make("edited-source");
        for manifest in ["Cargo.toml", "fuzz/Cargo.toml", "tests/fix/Cargo.toml"] {
            let path = dir.join(manifest);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "[package]\nname = \"x\"\n").unwrap();
        }
        configured(&dir, &["source"], |config| {
            config.not_shipped = Some(vec!["fuzz".to_string()]);
        });
        let told = |shown: &str, src: &str| -> Vec<String> {
            let file = dir.join(shown).to_string_lossy().into_owned();
            let rules = rules_for(Some(&dir), &file);
            faults(&file, src, &rules)
                .iter()
                .filter_map(|finding| finding.item.clone())
                .collect()
        };
        let panics =
            "#[allow(clippy::expect_used, reason = \"a fuzz target panics\")]\nfn f() {}\n";
        let unreasoned = "#[allow(dead_code)]\nfn f() {}\n";
        assert_eq!(
            [
                told("src/lib.rs", panics),
                told("fuzz/src/lib.rs", panics),
                told("src/lib.rs", unreasoned),
                told("fixtures/density.rs", unreasoned),
                told("tests/fix/src/lib.rs", unreasoned),
                told("fuzz/src/lib.rs", unreasoned),
            ],
            [
                vec!["safety_lint_allowed_in_shipped_code".to_string()],
                Vec::new(),
                vec!["unreasoned_allow_attribute".to_string()],
                Vec::new(),
                Vec::new(),
                vec!["unreasoned_allow_attribute".to_string()],
            ]
        );
    }

    /// The editor hears about a refused phrase in any language chock reads, not only in Rust.
    #[test]
    fn a_forbidden_phrase_is_reported_as_the_file_is_written() {
        let listed = [crate::project::config::Forbidden {
            text: "legacy shim".to_string(),
            why: "delete it or name what it does".to_string(),
            ..crate::project::config::Forbidden::default()
        }];
        for (path, src) in [
            ("src/lib.rs", "// a Legacy Shim\n"),
            ("Cargo.toml", "# a Legacy Shim\n"),
        ] {
            let rules = Rules {
                forbidden: listed.to_vec(),
                ..Rules::default()
            };
            assert_eq!(
                faults(path, src, &rules)
                    .iter()
                    .map(Finding::render)
                    .collect::<Vec<_>>(),
                [format!(
                    "{path}:1: legacy shim: delete it or name what it does"
                )],
                "{path}"
            );
        }
    }

    /// The editor names the file by its absolute path; `except` is written from the project root.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn phrases_come_from_the_project_config_less_those_it_allows_in_that_file() {
        let dir = crate::testdir::make("edited-forbidden");
        let named = |path: &str| dir.join(path).to_string_lossy().into_owned();
        let phrases = |root: Option<&std::path::Path>, path: &str| rules_for(root, path).forbidden;
        assert_eq!(phrases(None, "src/lib.rs"), Vec::new());
        assert_eq!(phrases(Some(&dir), &named("src/lib.rs")), Vec::new());
        let listed = vec![crate::project::config::Forbidden {
            text: "§".to_string(),
            why: "cite the file".to_string(),
            except: vec!["plans/**".to_string()],
            ..crate::project::config::Forbidden::default()
        }];
        configured(&dir, &["phrases"], |config| {
            config.forbidden = Some(listed.clone())
        });
        assert_eq!(phrases(Some(&dir), &named("src/lib.rs")), listed);
        assert_eq!(phrases(Some(&dir), &named("plans/083.md")), Vec::new());
    }

    fn configured(
        dir: &std::path::Path,
        gates: &[&str],
        shape: impl FnOnce(&mut crate::project::config::Config),
    ) {
        let mut config = crate::project::config::Config::of(gates.iter().copied());
        shape(&mut config);
        let path = dir.join(crate::project::config::FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_edited_file_answers_to_the_project_it_sits_in_not_the_session() {
        let tree = [
            "/r/Cargo.toml",
            "/r/crates/set/Cargo.toml",
            "/r/crates/set/.chock/config.json",
            "/r/crates/set/member/Cargo.toml",
        ];
        let project = |file: &str| {
            project_with(std::path::Path::new(file), &|path| {
                tree.iter().any(|held| std::path::Path::new(held) == path)
            })
        };
        let at = |dir: &str| Some(std::path::PathBuf::from(dir));
        assert_eq!(project("/r/crates/set/src/a.rs"), at("/r/crates/set"));
        assert_eq!(
            project("/r/crates/set/member/src/a.rs"),
            at("/r/crates/set"),
            "a workspace member answers to the root chock was set up in"
        );
        assert_eq!(
            project("/r/src/a.rs"),
            at("/r"),
            "not set up: its own crate"
        );
        assert_eq!(project("/elsewhere/a.rs"), None);
        assert_eq!(project("/"), None);
        let dir = crate::testdir::make("edited-project");
        configured(&dir, &["slop"], |_| ());
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let file = dir.join("src/a.rs").to_string_lossy().into_owned();
        assert_eq!(project_of(&file), Some(dir.to_path_buf()));
    }

    #[test]
    fn findings_are_told_one_to_a_line() {
        let found = faults(
            "src/lib.rs",
            "// one\n// two\n// three\n",
            &Rules::default(),
        );
        assert_eq!(told(&found), format!("  {}\n", found[0].render()));
        assert_eq!(told(&[]), "");
    }

    #[test]
    fn the_report_trips_exactly_when_the_file_had_something_to_say() {
        let clean = report(&[]);
        assert_eq!(
            (clean.gate.as_str(), clean.verdict, clean.findings),
            ("edited", crate::run::report::Verdict::Pass, Vec::new())
        );
        let found = faults(
            "src/lib.rs",
            "// one\n// two\n// three\n",
            &Rules::default(),
        );
        let tripped = report(&found);
        assert_eq!(
            (tripped.verdict, tripped.rerun.as_str(), tripped.findings),
            (
                crate::run::report::Verdict::Tripped,
                "chock edited <path>",
                found
            )
        );
    }

    #[test]
    #[cfg_attr(
        all(miri, target_os = "macos"),
        ignore = "Miri cannot set a file's time on macOS"
    )]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_second_firing_moves_the_record_the_first_one_left() {
        let dir = crate::testdir::make("edited-heartbeat");
        note_firing(&dir);
        let first = std::fs::read_to_string(dir.join(HEARTBEAT)).unwrap();

        // Backdated, so the test need not sleep.
        let path = dir.join(HEARTBEAT);
        let long_ago =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert!(since_firing(&dir).unwrap() > std::time::Duration::from_secs(60 * 60));

        note_firing(&dir);
        assert!(
            since_firing(&dir).unwrap() < std::time::Duration::from_secs(60),
            "the second firing left the record where the first one put it"
        );
        assert_ne!(
            std::fs::read_to_string(&path).unwrap(),
            first.replace(|c: char| c.is_ascii_digit(), "x"),
            "the contents carry the time, so two firings differ"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_whose_hook_never_fired_reports_no_firing_at_all() {
        let dir = crate::testdir::make("edited-no-heartbeat");
        assert_eq!(since_firing(&dir), None);
    }

    #[test]
    fn the_file_a_hook_names_is_read_out_of_the_tools_own_json() {
        let call = r#"{"tool_name":"Edit","tool_input":{"file_path":"src/a.rs"}}"#;
        assert_eq!(edited_path(call), Ok(Some("src/a.rs".to_string())));
    }

    /// Copilot CLI spells the same fields in camelCase.
    #[test]
    fn a_host_spelling_the_same_fields_the_other_way_is_read_the_same() {
        let camel = r#"{"toolName":"Edit","toolInput":{"filePath":"src/a.rs"}}"#;
        assert_eq!(edited_path(camel), Ok(Some("src/a.rs".to_string())));
    }

    /// Any editor can use the hook by piping the bare path.
    #[test]
    fn a_hook_that_names_the_path_and_nothing_else_is_read_the_same_way() {
        assert_eq!(edited_path("src/a.rs\n"), Ok(Some("src/a.rs".to_string())));
    }

    #[test]
    fn a_write_chock_has_no_rule_for_names_no_file_and_is_not_a_failure() {
        let image = r#"{"tool_name":"Write","tool_input":{"file_path":"logo.png"}}"#;
        assert_eq!(edited_path(image), Ok(None));
    }

    #[test]
    fn a_hook_that_named_no_file_is_refused_rather_than_passing_every_edit() {
        for said in [
            r#"{"tool_name":"Bash"}"#,
            r#"{"tool_input":{"command":"ls"}}"#,
            "not a path at all",
            "",
        ] {
            let err = edited_path(said).unwrap_err();
            assert!(
                err.starts_with("the hook named no file to check"),
                "{said}: {err}"
            );
        }
    }

    #[test]
    fn a_refusal_quotes_what_the_hook_actually_said() {
        let err = edited_path(r#"{"tool_name":"Bash"}"#).unwrap_err();
        assert!(err.ends_with(r#"was given: {"tool_name":"Bash"}"#), "{err}");
    }

    #[test]
    fn a_file_with_nothing_wrong_in_it_says_nothing() {
        assert_eq!(
            rendered("src/a.rs", "/// One line.\nfn f() -> u8 {\n    1\n}\n"),
            Vec::<String>::new()
        );
    }

    /// The usual cause: an item inserted between a doc comment and its item merges two blocks.
    #[test]
    fn a_comment_block_over_the_limit_is_reported_at_its_first_line() {
        let src = "/// One.\n/// Two.\n/// Three.\nfn f() {}\n";
        assert_eq!(
            rendered("src/a.rs", src),
            ["src/a.rs:1: comment block of 3 lines, over 2"]
        );
    }

    #[test]
    fn a_function_nested_past_the_limit_is_named_with_its_depth() {
        let src = "fn f() {\n if a {\n  if b {\n   if c {\n    if d {\n     g();\n}}}}}\n";
        let found = rendered("src/a.rs", src);
        assert!(
            found.iter().any(|f| f.contains("nested 5 deep")),
            "{found:?}"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_its_own_finding() {
        assert_eq!(
            rendered("src/a.rs", "fn f( {\n"),
            ["src/a.rs: does not parse: line 1: cannot parse string into token stream"]
        );
    }

    #[test]
    fn a_file_in_a_language_chock_does_not_know_is_left_alone() {
        assert_eq!(
            rendered("notes.txt", "some text\nmore text\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_comment_block_in_a_manifest_is_read_with_its_own_marker() {
        let src = "# One.\n# Two.\n# Three.\n[package]\n";
        assert_eq!(
            rendered("Cargo.toml", src),
            ["Cargo.toml:1: comment block of 3 lines, over 2"]
        );
    }
}
