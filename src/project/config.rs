//! Which gates this project has switched on, and how chock runs them, from `.chock/config.json`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::project::document::{self, Versioned};

pub const SCHEMA: u32 = 1;

pub const FILE: &str = ".chock/config.json";

/// An unknown key is refused, as the schema says, so a typo or a retired key is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The schema URL for this chock's version, recomputed on read rather than round-tripped.
    #[serde(
        rename = "$schema",
        default = "schema_url",
        deserialize_with = "recomputed"
    )]
    pub schema_url: String,
    pub version: u32,
    /// The gates switched on, sorted so `init` writes the same bytes each time.
    pub enabled: BTreeSet<String>,
    /// Default gates switched off, with why. `doctor` names a default gate off without an entry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub left_off: BTreeMap<String, String>,
    /// The full command that runs the project's tests, where `cargo nextest run` cannot. It must
    /// run every test after a failure, as `--no-fail-fast` does, or a run lists only the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<Vec<String>>,
    /// The full command that writes the project's coverage report, where one cargo command cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<String>>,
    /// Limits on commit subject width and body length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// Workspace members that never ship, such as a fuzz harness, a benchmark or an xtask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_shipped: Option<Vec<String>>,
    /// The packages `miri` runs over, since Miri cannot emulate a `-sys` dependency's calls into C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miri: Option<Scope>,
    /// The features chock's own commands build with. A tool that cannot take them refuses rather
    /// than build another configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
    /// Build with no default features; combines with `features`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_default_features: Option<bool>,
    /// Build with every feature. Wins over `features`, as `--all-features` does in cargo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_features: Option<bool>,
    /// The target triple to build for, where not the host. A tool that cannot take it refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The profile the project ships, where not `release`. A tool that cannot take it refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Which system holds the tree's history, where both do: `git` or `outpost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<crate::project::vcs::Kind>,
    /// Gates the project runs at a stage other than their default; `chock stage` writes it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stage: BTreeMap<String, Stage>,
    /// The tools `runner` calls, which chock cannot see; naming them lets `test` recall a verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_tools: Option<Vec<String>>,
    /// The tools `coverage` calls, which chock cannot see; naming them lets `coverage` and `crap`
    /// recall a verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_tools: Option<Vec<String>>,
    /// A command the suite runs under, such as a script that starts a service the tests need. The
    /// suite, coverage and mutest commands follow it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_wrapper: Option<Vec<String>>,
    /// Whether mutest runs every test on each mutation, so a test that detects none is named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutest_exhaustive: Option<bool>,
    /// Flags mutest takes after chock's own, such as `--timings`; the results directory then stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutest_args: Option<Vec<String>>,
    /// The most Kani harnesses `proof` verifies at once; each solver can hold several GB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_jobs: Option<u32>,
    /// Settings of the `proof` gate: the packages Kani verifies, and how long it may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
    /// Checks whose tool CI cannot install: the hooks still run them, `chock run --ci` does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only: Option<Vec<String>>,
    /// Ratchets held to zero rather than to the baseline, so any finding fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<Vec<String>>,
    /// Ratchets held to zero in each file the change touched, so debt goes when its file is edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean_when_touched: Option<Vec<String>>,
    /// The project's own checks, run as gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<Command>>,
    /// Phrases this project refuses anywhere in its source, each with what a writer is told instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forbidden: Option<Vec<Forbidden>>,
    /// Rust files `mutest` leaves alone, each with the reason no test need judge its mutations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_mutated: Option<Vec<Unmutated>>,
    /// Settings of the `history` gate: the leaks it no longer reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<History>,
    /// Paths and crates only some files or packages may use, which the `seams` gate holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seams: Option<Vec<Seam>>,
    /// Calls whose first argument one file reads, which the `owners` gate holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owners: Option<Vec<Owner>>,
    /// Calls that need another call in the same function, which the `calls` gate holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<Paired>>,
    /// The project's own credential shapes, which `hygiene` and `history` find with their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<Secret>>,
    /// Settings of the `tla` gate: where the models are, and how long TLC may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tla: Option<Tla>,
    /// Settings of the `verus` gate: the files Verus must verify, and the mutants it must refute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verus: Option<Verus>,
}

/// One declared check. A pass/fail command fails its gate by exiting non-zero; a counting one
/// prints `{"key": n}` and each key is ratcheted. `builds` moves it from the commit to the push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    pub run: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub counts: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builds: bool,
    /// The files or directories, from the root, that the command writes. Its gate fails at each
    /// one whose bytes the run changed, so a committed generated file is never stale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

/// A phrase refused in source, matched without regard to case, and the reason given with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forbidden {
    pub text: String,
    pub why: String,
    /// Only where it stands alone: `dvc` in `advc`, `dvc-core` or `.dvc` is not a mention.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub word: bool,
    /// Paths from the project root where the phrase is allowed; `*` is one segment, `**` any number.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub except: Vec<String>,
    /// Paths from the project root where the phrase is refused; none is every path.
    #[serde(default, rename = "in", skip_serializing_if = "Vec::is_empty")]
    pub within: Vec<String>,
}

/// A Rust path, or a crate, that only the files or packages under `only` may use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seam {
    /// A path such as `iroh` or `std::process::Command`; a use of it or of a path under it counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A crate no package outside `only` may hold in its normal dependency tree.
    #[serde(default, rename = "crate", skip_serializing_if = "Option::is_none")]
    pub krate: Option<String>,
    /// Files from the root for a path, with `*` and `**`; package names for a crate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    pub why: String,
}

/// A call whose first argument names a value, such as an environment variable, that one file reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    /// The full path of the function, such as `std::env::var`.
    pub call: String,
    pub why: String,
}

/// A call that needs another in the same function: earlier there with `after`, anywhere with `with`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paired {
    pub call: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<String>,
    pub why: String,
}

/// A credential shape of the project's own: a fixed prefix, then at least `tail` letters, digits,
/// `_` or `-`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub name: String,
    pub prefix: String,
    pub tail: usize,
}

/// A file `mutest` does not mutate, such as generated code, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unmutated {
    pub path: String,
    pub why: String,
}

/// What the `proof` gate verifies. A field left out keeps chock's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    /// The packages Kani lists and verifies, each as `-p`; the whole workspace when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<String>>,
    /// Seconds the Kani run of every harness may take, and of each mutant; chock's tool limit
    /// when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    /// The directory of mutant patches, each of which a harness must fail on;
    /// `proofs/kani/mutants` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants: Option<String>,
    /// Whether each harness must declare a `kani::cover!`; not when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_cover: Option<bool>,
}

/// What the `verus` gate verifies. A field left out keeps chock's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verus {
    /// Paths or globs from the root of the files Verus must verify; `proofs/verus/*.rs` when left
    /// out. A crate's `lib.rs` verifies its modules with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// The directory of mutant patches, each of which Verus must refute; `proofs/verus/mutants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants: Option<String>,
    /// The arguments each Verus run takes after the file; `["--crate-type=lib"]` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Seconds one Verus run may take; 600 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

/// How the `tla` gate checks TLA+ models. A field left out keeps chock's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tla {
    /// Directories, from the root, whose `.cfg` files TLC checks; `proofs/tla` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirs: Option<Vec<String>>,
    /// Worker threads of one TLC run, and of one tlapm run; 2 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u32>,
    /// Seconds one cfg, one Apalache check, or the tlapm proof of one module may run; 300 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    /// Seconds for the cfgs of one model, or for one cfg, by the model's name or the cfg's stem.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub timeouts: BTreeMap<String, u64>,
    /// The models Apalache proves inductive, each with its inductive invariant. Left out, every
    /// model whose own cfg lists `IndInv`; `{}` proves none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inductive: Option<BTreeMap<String, String>>,
    /// Steps of a bounded Apalache check from `Init` after the proof; none when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    /// The SMT encoding of Apalache, by the model's name, as `--smt-encoding` names it; Apalache's
    /// own when left out. The `tla` gate refuses a name that Apalache does not take.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub smt_encoding: BTreeMap<String, String>,
}

/// What the `history` gate accepts as it reads every commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted: Vec<Accepted>,
}

/// A credential published on purpose, such as a test key: what `rule` found in `path`, at the
/// commit `chock run history` names, or a start of that commit's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    pub commit: String,
    pub path: String,
    pub rule: String,
    pub reason: String,
}

/// The packages a gate runs over; neither `packages` nor `exclude` means the whole workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// The interpreter's flags, replacing chock's rather than adding, so one can be dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<Vec<String>>,
}

/// Commit message limits; an absent field keeps chock's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<usize>,
}

fn schema_url() -> String {
    document::schema_url("config", SCHEMA)
}

/// Reads past the file's `$schema`, which names the chock that wrote it, not this one.
fn recomputed<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(schema_url())
}

/// The first step that runs a gate; each later step runs it too. `manual` is `chock run` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Commit,
    Push,
    Ci,
    Manual,
}

impl Stage {
    pub const ALL: [Stage; 4] = [Stage::Commit, Stage::Push, Stage::Ci, Stage::Manual];

    /// A gate that needs no compiler answers in seconds, so the commit waits for it.
    #[must_use]
    pub fn default_for(builds: bool) -> Self {
        if builds { Stage::Push } else { Stage::Commit }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Stage::Commit => "commit",
            Stage::Push => "push",
            Stage::Ci => "ci",
            Stage::Manual => "manual",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Stage::ALL.into_iter().find(|stage| stage.name() == name)
    }
}

impl Versioned for Config {
    const SCHEMA: u32 = SCHEMA;
    const FILE: &'static str = FILE;

    fn version(&self) -> u32 {
        self.version
    }

    fn describe() -> &'static str {
        "a chock config"
    }
}

impl Config {
    #[must_use]
    pub fn of<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Self {
            schema_url: schema_url(),
            version: SCHEMA,
            enabled: names.into_iter().map(Into::into).collect(),
            left_off: BTreeMap::new(),
            runner: None,
            coverage: None,
            message: None,
            not_shipped: None,
            miri: None,
            features: None,
            no_default_features: None,
            all_features: None,
            target: None,
            profile: None,
            vcs: None,
            stage: BTreeMap::new(),
            runner_tools: None,
            coverage_tools: None,
            test_wrapper: None,
            mutest_exhaustive: None,
            mutest_args: None,
            proof_jobs: None,
            proof: None,
            local_only: None,
            strict: None,
            clean_when_touched: None,
            commands: None,
            forbidden: None,
            not_mutated: None,
            history: None,
            tla: None,
            verus: None,
            seams: None,
            owners: None,
            calls: None,
            secrets: None,
        }
    }

    /// `{lcov}` in the argv marks where chock reads the report from.
    #[must_use]
    pub fn with_coverage<I: IntoIterator<Item = S>, S: Into<String>>(mut self, argv: I) -> Self {
        self.coverage = Some(argv.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn is_on(&self, gate: &str) -> bool {
        self.enabled.contains(gate)
    }

    /// The stage the project chose for a gate, else the gate's default.
    #[must_use]
    pub fn stage_of(&self, gate: &str, builds: bool) -> Stage {
        self.stage
            .get(gate)
            .copied()
            .unwrap_or(Stage::default_for(builds))
    }

    /// Records a gate's stage; its default stage removes the entry. `true` when this changed it.
    pub fn place(&mut self, gate: &str, builds: bool, stage: Stage) -> bool {
        let was = self.stage_of(gate, builds);
        if stage == Stage::default_for(builds) {
            self.stage.remove(gate);
        } else {
            self.stage.insert(gate.to_string(), stage);
        }
        was != stage
    }

    /// The configured features as cargo flags. `--all-features` wins over named features.
    #[must_use]
    pub fn cargo_features(&self) -> Vec<String> {
        let mut asked = Vec::new();
        if self.no_default_features == Some(true) {
            asked.push("--no-default-features".to_string());
        }
        if self.all_features == Some(true) {
            asked.push("--all-features".to_string());
            return asked;
        }
        if let Some(named) = self.features.as_ref().filter(|named| !named.is_empty()) {
            asked.push("--features".to_string());
            asked.push(named.join(","));
        }
        asked
    }

    /// The target and profile in cargo's own spelling, for the tools that take cargo's flags.
    #[must_use]
    pub fn cargo_build(&self) -> Vec<String> {
        let mut asked = Vec::new();
        for (flag, value) in [("--target", &self.target), ("--profile", &self.profile)] {
            if let Some(value) = value {
                asked.extend([flag.to_string(), value.clone()]);
            }
        }
        asked
    }

    /// `true` when this changed the set, so a caller can tell "switched on" from "already on".
    pub fn enable(&mut self, gate: &str) -> bool {
        self.left_off.remove(gate);
        self.enabled.insert(gate.to_string())
    }

    /// Switches a gate off and records why in `left_off`; no reason records a hand decision.
    pub fn disable(&mut self, gate: &str, why: Option<&str>) -> bool {
        let why = why.unwrap_or("switched off by hand");
        self.left_off.insert(gate.to_string(), why.to_string());
        self.enabled.remove(gate)
    }

    /// Default gates neither on nor recorded as left off.
    #[must_use]
    pub fn undecided<'a>(&self, defaults: &[&'a str]) -> Vec<&'a str> {
        defaults
            .iter()
            .copied()
            .filter(|gate| !self.is_on(gate) && !self.left_off.contains_key(*gate))
            .collect()
    }

    /// Configured gate names no gate answers to, such as a gate renamed between chock versions.
    #[must_use]
    pub fn unknown(&self, known: &[&str]) -> Vec<String> {
        self.enabled
            .iter()
            .chain(self.strict.iter().flatten())
            .chain(self.clean_when_touched.iter().flatten())
            .chain(self.stage.keys())
            .filter(|name| !known.contains(&name.as_str()))
            .cloned()
            .collect()
    }

    /// Names under `clean_when_touched` that `holds` refuses. A gate with no number for each file
    /// would pass that list in silence, so the run refuses the config.
    #[must_use]
    pub fn unheld_where_touched(&self, holds: &dyn Fn(&str) -> bool) -> Vec<String> {
        self.clean_when_touched
            .iter()
            .flatten()
            .filter(|name| !holds(name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn every_switch_is_a_recorded_decision_and_only_an_unrecorded_gap_is_undecided() {
        let mut config = Config::of(["lint"]);
        let defaults = ["lint", "slop", "typos"];
        assert_eq!(config.undecided(&defaults), ["slop", "typos"]);
        assert!(config.disable("lint", None));
        assert_eq!(
            config.undecided(&defaults),
            ["slop", "typos"],
            "a decision, not a gap"
        );
        assert_eq!(config.left_off["lint"], "switched off by hand");
        assert!(
            !config.disable("lint", Some("too slow here")),
            "already off"
        );
        assert_eq!(
            config.left_off["lint"], "too slow here",
            "a given reason replaces it"
        );
        assert!(config.enable("slop"));
        assert!(!config.enable("slop"), "already on");
        assert_eq!(config.undecided(&defaults), ["typos"]);
        assert!(config.enable("lint"));
        assert_eq!(
            config.left_off,
            BTreeMap::new(),
            "switching on clears the record"
        );
    }

    /// A stage is recorded only where it is not the gate's default, so the config stays short.
    #[test]
    fn a_stage_is_recorded_only_where_it_is_not_the_default() {
        let mut config = Config::of(["binsize"]);
        assert_eq!(config.stage_of("binsize", true), Stage::Push);
        assert_eq!(config.stage_of("slop", false), Stage::Commit);
        assert!(config.place("binsize", true, Stage::Ci));
        assert!(!config.place("binsize", true, Stage::Ci), "already there");
        assert_eq!(config.stage_of("binsize", true), Stage::Ci);
        assert!(config.place("binsize", true, Stage::Push));
        assert!(config.stage.is_empty(), "the default needs no entry");
        assert!(
            !config.place("lint", true, Stage::Push),
            "a gate that builds starts at push"
        );
        assert!(
            Stage::ALL
                .iter()
                .all(|s| Stage::named(s.name()) == Some(*s))
        );
    }

    /// The schema refuses a key it does not declare, so chock does too, and names the key.
    #[test]
    fn a_key_the_schema_does_not_declare_is_refused_by_name() {
        let retired = r#"{"version": 1, "enabled": ["binsize"], "at_ci": ["binsize"]}"#;
        let refused = document::parse::<Config>(retired, FILE)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("unknown field `at_ci`"), "{refused}");
        let staged =
            r#"{"$schema": "x", "version": 1, "enabled": [], "stage": {"binsize": "manual"}}"#;
        let read = document::parse::<Config>(staged, FILE).unwrap();
        assert_eq!(read.stage_of("binsize", true), Stage::Manual);
        assert_eq!(
            read.schema_url,
            schema_url(),
            "a file's `$schema` is recomputed"
        );
        assert_eq!(
            read.unknown(&[]),
            ["binsize"],
            "a staged name must be a gate"
        );
    }

    #[test]
    fn a_target_and_profile_are_spelled_as_cargo_spells_them() {
        let config = Config {
            target: Some("wasm32-wasip1".to_string()),
            profile: Some("dist".to_string()),
            ..Config::of(["lint"])
        };
        assert_eq!(
            config.cargo_build(),
            ["--target", "wasm32-wasip1", "--profile", "dist"]
        );
        assert_eq!(Config::of(["lint"]).cargo_build(), Vec::<String>::new());
    }

    #[test]
    fn a_gate_in_the_set_is_on_and_one_outside_it_is_not() {
        let config = Config::of(["lint", "slop"]);
        assert!(config.is_on("lint"));
        assert!(config.is_on("slop"));
        assert!(!config.is_on("mutation"));
    }

    #[test]
    fn a_config_round_trips_through_its_file_format() {
        let config = Config::of(["lint", "slop"]);
        assert_eq!(
            document::parse::<Config>(&document::render(&config), "c").unwrap(),
            config
        );
    }

    #[test]
    fn the_rendered_file_is_sorted_so_two_runs_of_init_agree() {
        let text = document::render(&Config::of(["slop", "lint", "deps"]));
        assert!(text.find("\"deps\"") < text.find("\"lint\""));
        assert!(text.find("\"lint\"") < text.find("\"slop\""));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn a_name_no_gate_answers_to_is_reported_rather_than_run_silently() {
        let config = Config::of(["lint", "codeslope"]);
        assert_eq!(
            config.unknown(&["lint", "codeslop"]),
            vec!["codeslope".to_string()]
        );
    }

    /// Lower-case, as a person writes it in the file.
    #[test]
    fn a_config_naming_the_system_that_holds_the_tree_is_read_as_that_system() {
        let read = |text: &str| document::parse::<Config>(text, "config.json").map(|set| set.vcs);
        let named = r#"{"version": 1, "enabled": [], "vcs": "outpost"}"#;
        assert_eq!(
            read(named).unwrap(),
            Some(crate::project::vcs::Kind::Outpost)
        );
        let unnamed = r#"{"version": 1, "enabled": []}"#;
        assert_eq!(read(unnamed).unwrap(), None);
        let wrong = r#"{"version": 1, "enabled": [], "vcs": "mercurial"}"#;
        assert!(read(wrong).is_err(), "an unknown system was accepted");
    }

    #[test]
    fn a_config_naming_only_real_gates_reports_nothing_unknown() {
        assert!(Config::of(["lint"]).unknown(&["lint", "slop"]).is_empty());
    }

    #[test]
    fn a_strict_name_no_gate_answers_to_is_reported_like_an_enabled_one() {
        let mut config = Config::of(["slop"]);
        config.strict = Some(vec!["slop".to_string(), "sloop".to_string()]);
        config.clean_when_touched = Some(vec!["slop".to_string(), "slp".to_string()]);
        assert_eq!(config.unknown(&["slop"]), ["sloop", "slp"]);
    }

    #[test]
    fn a_name_held_where_touched_that_keeps_no_number_for_each_file_is_reported() {
        let mut config = Config::of(["slop", "binsize"]);
        assert!(config.unheld_where_touched(&|_| false).is_empty());
        config.clean_when_touched = Some(vec!["slop".to_string(), "binsize".to_string()]);
        assert_eq!(
            config.unheld_where_touched(&|name| name == "slop"),
            ["binsize"]
        );
        assert!(config.unheld_where_touched(&|_| true).is_empty());
    }

    #[test]
    fn the_features_a_project_names_are_spelled_the_way_cargo_spells_them() {
        let named = Config {
            features: Some(vec!["testkit".to_string(), "tabular".to_string()]),
            ..Config::of(["lint"])
        };
        assert_eq!(named.cargo_features(), ["--features", "testkit,tabular"]);
    }

    #[test]
    fn asking_for_every_feature_wins_over_naming_some() {
        let both = Config {
            features: Some(vec!["testkit".to_string()]),
            all_features: Some(true),
            no_default_features: Some(true),
            ..Config::of(["lint"])
        };
        assert_eq!(
            both.cargo_features(),
            ["--no-default-features", "--all-features"]
        );
    }

    #[test]
    fn a_project_naming_no_features_asks_cargo_for_nothing() {
        assert_eq!(Config::of(["lint"]).cargo_features(), Vec::<String>::new());
        let empty = Config {
            features: Some(Vec::new()),
            ..Config::of(["lint"])
        };
        assert_eq!(empty.cargo_features(), Vec::<String>::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_missing_config_is_absence_rather_than_failure() {
        let dir = crate::testdir::make("config-missing");
        assert_eq!(document::read::<Config>(&dir), Ok(None));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_written_config_reads_back_from_its_project_root() {
        let dir = crate::testdir::make("config-roundtrip");
        let config = Config::of(["lint"]);
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        std::fs::write(dir.join(FILE), document::render(&config)).unwrap();
        assert_eq!(document::read::<Config>(&dir), Ok(Some(config)));
    }

    #[test]
    fn a_config_from_a_newer_chock_is_refused_rather_than_half_read() {
        let text = r#"{"version":99,"enabled":[]}"#;
        assert_eq!(
            document::parse::<Config>(text, "c.json"),
            Err(document::Error::FromTheFuture {
                path: "c.json".to_string(),
                found: 99,
                reads: SCHEMA
            })
        );
    }

    /// `runner` is optional, so older configs still parse.
    #[test]
    fn a_config_that_names_no_runner_reads_as_the_default() {
        let config = document::parse::<Config>(r#"{"version":1,"enabled":[]}"#, "c.json").unwrap();
        assert_eq!(config.runner, None);
    }

    #[test]
    fn the_command_a_project_runs_its_tests_with_survives_a_round_trip() {
        let written = Config {
            runner: Some(vec![
                "scripts/run-tests".to_string(),
                "--serial".to_string(),
            ]),
            ..Config::of(["test"])
        };
        let read = document::parse::<Config>(&document::render(&written), "c.json").unwrap();
        assert_eq!(
            read.runner,
            Some(vec![
                "scripts/run-tests".to_string(),
                "--serial".to_string()
            ])
        );
    }

    /// A custom command with no tools named has no verdict key, so a lost list is a slow run.
    #[test]
    fn the_tools_a_project_names_for_its_commands_survive_a_round_trip() {
        let written = Config {
            runner_tools: Some(vec!["cargo-nextest".to_string()]),
            coverage_tools: Some(vec!["cargo-llvm-cov".to_string()]),
            ..Config::of(["test"])
        };
        let read = document::parse::<Config>(&document::render(&written), "c.json").unwrap();
        assert_eq!(read, written);
        assert!(!document::render(&Config::of(["test"])).contains("_tools"));
    }

    #[test]
    fn a_config_on_the_default_runner_writes_no_runner_key() {
        assert!(!document::render(&Config::of(["test"])).contains("runner"));
    }

    #[test]
    fn text_that_is_not_a_config_names_the_file_it_came_from() {
        let err = document::parse::<Config>("not json", "c.json").unwrap_err();
        assert!(matches!(err, document::Error::Unparsable { path, .. } if path == "c.json"));
    }
}
