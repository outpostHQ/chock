//! What every gate is handed: the root, the record, and what the project's config decides.

use std::path::PathBuf;

use super::FirstRun;
use crate::gates;
use crate::project::config::Config;
use crate::run::baseline::Baseline;

/// What every gate is handed, filled once from the config and the machine, so a test can set any
/// of it. A gate edits no source; `init`'s ignores cover what its tool leaves behind.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub root: PathBuf,
    pub baseline: Baseline,
    /// How many chock runs this one is nested inside.
    pub depth: u32,
    /// How many jobs a spawned tool may run at once, from this machine's memory, not its cores.
    pub jobs: usize,
    /// The command that runs the suite, and the tools it calls.
    pub runner: Runner,
    /// The command that writes the coverage report.
    pub coverage: Coverage,
    /// How wide a commit subject and how long its body may be.
    pub message: crate::gates::repo::commits::Limits,
    /// Members nobody receives, such as fuzz harnesses and benchmarks.
    pub not_shipped: Vec<String>,
    /// One `outpost check --json` shared by every gate in a run, so the tree is read once.
    pub checked: std::sync::Arc<std::sync::OnceLock<Result<crate::gates::outpost::Check, String>>>,
    /// Which packages `miri` runs over; empty is the whole workspace.
    pub miri: crate::project::config::Scope,
    /// Every file under the root, walked once and shared by every gate.
    pub listing: std::sync::Arc<std::sync::OnceLock<Result<crate::project::Listing, String>>>,
    /// Which repository's history the gates read, where both git and Outpost hold the tree.
    pub vcs: Option<crate::project::vcs::Kind>,
    /// The suite's first run, shared by `test` and `idempotent` so a green tree runs it once.
    pub suite: FirstRun,
    /// The feature flags cargo builds with.
    pub features: Vec<String>,
    /// The target and profile flags in cargo's spelling; empty is the host and default profiles.
    pub build: Vec<String>,
    /// The project's own declared checks.
    pub commands: Vec<crate::project::config::Command>,
    /// Phrases the project refuses in its source.
    pub forbidden: Vec<crate::project::config::Forbidden>,
    /// Leaks the project reviewed and accepted, which `history` does not report.
    pub accepted: Vec<crate::project::config::Accepted>,
    /// Ratchets held to zero rather than to their record.
    pub strict: Vec<String>,
    /// Ratchets held to zero in each file the change touched.
    pub clean_when_touched: Vec<String>,
    /// The files the change touched, read once where a gate first asks.
    pub changed: std::sync::Arc<std::sync::OnceLock<Result<Vec<String>, String>>>,
    /// Whether this is CI's run, which writes no record and fails a gain the record does not hold.
    pub ci: bool,
    /// Whether a gate that narrows a local run to the change must measure the whole tree.
    pub whole: bool,
    /// Whether every gate is judged again, with no verdict recalled; the new one is still kept.
    pub no_cache: bool,
    /// The part of the Miri suite this run takes, from `--miri-partition`; `None` is all of it.
    pub miri_part: Option<crate::gates::tools::miri::Part>,
}

impl Ctx {
    /// Every capability at its neutral setting. A run overrides it from the config; a test sets
    /// what it needs.
    #[must_use]
    pub fn for_root(root: PathBuf, baseline: Baseline) -> Self {
        Self {
            root,
            baseline,
            depth: 0,
            jobs: 1,
            runner: default_runner(),
            coverage: default_coverage(),
            message: crate::gates::repo::commits::Limits::default(),
            not_shipped: Vec::new(),
            checked: std::sync::Arc::default(),
            miri: crate::project::config::Scope::default(),
            listing: std::sync::Arc::default(),
            vcs: None,
            suite: std::sync::Arc::default(),
            features: Vec::new(),
            build: Vec::new(),
            commands: Vec::new(),
            forbidden: Vec::new(),
            accepted: Vec::new(),
            strict: Vec::new(),
            clean_when_touched: Vec::new(),
            changed: std::sync::Arc::default(),
            ci: false,
            whole: false,
            no_cache: false,
            miri_part: None,
        }
    }

    /// Every field a project's config decides. The caller sets the root, the record, and the
    /// repository, without which `commits` and `hygiene` measure nothing.
    #[must_use]
    pub fn from_config(config: Option<&Config>) -> Self {
        Self {
            depth: crate::exec::depth(),
            jobs: crate::exec::budget::of_this_machine(crate::exec::budget::PER_JOB_MB),
            runner: runner_for(config),
            coverage: coverage_for(config),
            message: crate::gates::repo::commits::Limits::of(config.and_then(|set| set.message)),
            not_shipped: config
                .and_then(|set| set.not_shipped.clone())
                .unwrap_or_default(),
            miri: config.and_then(|set| set.miri.clone()).unwrap_or_default(),
            features: config.map(Config::cargo_features).unwrap_or_default(),
            build: config.map(Config::cargo_build).unwrap_or_default(),
            commands: config
                .and_then(|set| set.commands.clone())
                .unwrap_or_default(),
            forbidden: config
                .and_then(|set| set.forbidden.clone())
                .unwrap_or_default(),
            accepted: config
                .and_then(|set| set.history.clone())
                .unwrap_or_default()
                .accepted,
            strict: config
                .and_then(|set| set.strict.clone())
                .unwrap_or_default(),
            clean_when_touched: config
                .and_then(|set| set.clean_when_touched.clone())
                .unwrap_or_default(),
            ..Self::for_root(PathBuf::new(), Baseline::default())
        }
    }

    /// The record a gate's sites are named against: none where the project holds it to zero.
    #[must_use]
    pub fn record(&self, gate: &str) -> crate::run::baseline::Series {
        if self.strict.iter().any(|name| name == gate) {
            return crate::run::baseline::Series::default();
        }
        self.baseline.gate(gate)
    }

    /// `record`, less each file this change touched where `clean_when_touched` holds the gate to
    /// zero, so every site in such a file is named.
    #[must_use]
    pub fn named_against(&self, gate: &str) -> crate::run::baseline::Series {
        let mut held = self.record(gate);
        if self.clean_when_touched.iter().any(|name| name == gate) {
            let touched = self.changed().unwrap_or_default();
            held.0.retain(|key, _| {
                let file = key.split_once('#').map_or(key.as_str(), |(path, _)| path);
                !touched.iter().any(|path| path == file)
            });
        }
        held
    }

    /// The files the change touched, from the repository that holds the tree.
    pub fn changed(&self) -> Result<&[String], String> {
        self.changed
            .get_or_init(|| crate::project::vcs::changed(&self.root, self.vcs, self.ci))
            .as_deref()
            .map_err(Clone::clone)
    }

    /// Features, then target and profile, for a tool that takes cargo's own flags.
    #[must_use]
    pub fn cargo_args(&self) -> Vec<&str> {
        self.features
            .iter()
            .chain(&self.build)
            .map(String::as_str)
            .collect()
    }

    /// One configured build flag's value, such as the triple after `--target`.
    #[must_use]
    pub fn build_flag(&self, flag: &str) -> Option<&str> {
        self.build
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    }

    /// Refused for a tool that cannot be told the configured target or profile: it would measure a
    /// build the project does not ship and report it as this one.
    pub fn default_build(&self, tool: &str) -> Result<(), String> {
        if self.build.is_empty() {
            return Ok(());
        }
        Err(format!(
            "{tool} cannot be told the configured `{}`, so it would measure another build",
            self.build.join(" ")
        ))
    }
}

/// Where chock reads the coverage report from, substituted into the command that writes it.
pub const LCOV: &str = "{lcov}";

/// What writes the report where a project does not name its own command. Enough for a suite that
/// runs in one process; a suite that drives its own binary instruments several and needs its own.
#[must_use]
pub fn default_coverage() -> Coverage {
    // Not `--all-targets`: a `harness = false` bench refuses nextest's listing.
    let argv = "cargo llvm-cov nextest --workspace --no-tests=pass --lcov --output-path";
    let argv: Vec<String> = argv.split(' ').chain([LCOV]).map(str::to_string).collect();
    Coverage {
        // chock's own command, so chock knows what it calls. A project with its own must say.
        tools: ["cargo", "cargo-llvm-cov", "cargo-nextest"]
            .map(str::to_string)
            .to_vec(),
        ..argv.into()
    }
}

/// The command and its invocation-local measurement stay together when a context is cloned.
#[derive(Debug, Clone)]
pub struct Coverage {
    pub argv: Vec<String>,
    /// The commands `argv` calls, which chock cannot see in a project's own command.
    pub tools: Vec<String>,
    pub(crate) report: std::sync::Arc<std::sync::OnceLock<Result<gates::coverage::Report, String>>>,
}

impl From<Vec<String>> for Coverage {
    fn from(argv: Vec<String>) -> Self {
        Self {
            argv,
            tools: Vec::new(),
            report: std::sync::Arc::default(),
        }
    }
}

/// What runs the suite where a project does not name its own command. `--no-fail-fast`, since
/// nextest otherwise stops at the first failure and the run would list fewer than exist.
#[must_use]
pub fn default_runner() -> Runner {
    let argv = "cargo nextest run --workspace --no-tests=fail --no-fail-fast";
    Runner {
        argv: argv.split(' ').map(str::to_string).collect(),
        // chock's own runner, so chock knows what it calls. A project with its own must say.
        tools: vec!["cargo".to_string(), "cargo-nextest".to_string()],
    }
}

/// Custom commands are complete argv; only commands chock owns receive Cargo feature flags.
#[must_use]
pub fn runner_for(config: Option<&crate::project::config::Config>) -> Runner {
    if let Some(argv) = config.and_then(|set| set.runner.clone()) {
        return Runner {
            argv,
            tools: config
                .and_then(|set| set.runner_tools.clone())
                .unwrap_or_default(),
        };
    }
    let mut runner = default_runner();
    runner
        .argv
        .extend(config.map(Config::cargo_features).unwrap_or_default());
    // nextest's own `--profile` is a nextest profile; cargo's is spelled `--cargo-profile` there.
    let build = config.map(Config::cargo_build);
    runner
        .argv
        .extend(build.unwrap_or_default().into_iter().map(|flag| {
            if flag == "--profile" {
                "--cargo-profile".to_string()
            } else {
                flag
            }
        }));
    runner
}

#[must_use]
pub fn coverage_for(config: Option<&crate::project::config::Config>) -> Coverage {
    if let Some(argv) = config.and_then(|set| set.coverage.clone()) {
        return Coverage {
            tools: config
                .and_then(|set| set.coverage_tools.clone())
                .unwrap_or_default(),
            ..argv.into()
        };
    }
    let mut coverage = default_coverage();
    coverage
        .argv
        .extend(config.map(Config::cargo_features).unwrap_or_default());
    // `cargo llvm-cov nextest` documents no way to be told either, so it would cover the host.
    if config.is_some_and(|set| !set.cargo_build().is_empty()) {
        coverage.report = std::sync::Arc::new(std::sync::OnceLock::from(Err(
            "the default coverage command cannot be told the configured target or profile; name \
             the command that writes the report in `coverage`"
                .to_string(),
        )));
    }
    coverage
}

/// What runs the suite, and the commands it calls that chock cannot see. A script is in the tree;
/// the clippy and kani it calls are not, so a change to one makes a kept verdict stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runner {
    pub argv: Vec<String>,
    pub tools: Vec<String>,
}

#[cfg(test)]
impl Ctx {
    /// A test's context at `root`, holding an empty record.
    pub(crate) fn at(root: &std::path::Path) -> Self {
        Self::for_root(root.to_path_buf(), Baseline::empty("0.1.0"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::config::{Accepted, History};

    #[test]
    fn the_leaks_a_project_accepted_reach_the_history_gate() {
        let fixture = Accepted {
            commit: "a389111".to_string(),
            path: "tests/fixtures/assertion.jwt".to_string(),
            rule: "jwt".to_string(),
            reason: "signed by the throwaway key beside it".to_string(),
        };
        let config = Config {
            history: Some(History {
                accepted: vec![fixture.clone()],
            }),
            ..Config::of(["history"])
        };
        assert_eq!(Ctx::from_config(Some(&config)).accepted, vec![fixture]);
        assert!(Ctx::from_config(None).accepted.is_empty());
    }

    /// A project's own command hides what it calls, so the names it gives are all a key holds.
    #[test]
    fn the_tools_a_project_names_for_its_own_commands_reach_the_run() {
        let named = Config {
            runner: Some(vec!["bin/test-rust".to_string()]),
            runner_tools: Some(vec!["cargo-nextest".to_string()]),
            coverage: Some(vec!["bin/coverage-rust".to_string(), LCOV.to_string()]),
            coverage_tools: Some(vec!["cargo-llvm-cov".to_string()]),
            ..Config::of(["test"])
        };
        assert_eq!(runner_for(Some(&named)).tools, ["cargo-nextest"]);
        assert_eq!(coverage_for(Some(&named)).tools, ["cargo-llvm-cov"]);
        let unnamed = Config {
            runner_tools: None,
            coverage_tools: None,
            ..named
        };
        assert!(runner_for(Some(&unnamed)).tools.is_empty());
        assert!(coverage_for(Some(&unnamed)).tools.is_empty());
    }

    /// chock knows what its own commands call, so a list for a command the project did not
    /// replace changes nothing.
    #[test]
    fn the_default_commands_keep_the_tools_chock_knows() {
        let listed = Config {
            runner_tools: Some(vec!["make".to_string()]),
            coverage_tools: Some(vec!["make".to_string()]),
            ..Config::of(["test"])
        };
        assert_eq!(runner_for(Some(&listed)).tools, default_runner().tools);
        assert_eq!(coverage_for(Some(&listed)).tools, default_coverage().tools);
        assert_eq!(
            default_coverage().tools,
            ["cargo", "cargo-llvm-cov", "cargo-nextest"]
        );
    }
}
