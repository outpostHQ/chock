//! One build in directories of its own, and the commands it runs there.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Answer, Plan, Ran, Rule, Scenario, TIMEOUT, copied, literal, tree};
use crate::exec::fixed;

/// The same clock, language and author for both builds. A scenario's own `env` comes after.
const FIXED: [(&str, &str); 19] = [
    ("TZ", "UTC"),
    ("LC_ALL", "C"),
    ("LANG", "C"),
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("SOURCE_DATE_EPOCH", "1700000000"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "chock oracle"),
    ("GIT_AUTHOR_EMAIL", "oracle@chock.invalid"),
    ("GIT_AUTHOR_DATE", "2023-11-14T22:13:20Z"),
    ("GIT_COMMITTER_NAME", "chock oracle"),
    ("GIT_COMMITTER_EMAIL", "oracle@chock.invalid"),
    ("GIT_COMMITTER_DATE", "2023-11-14T22:13:20Z"),
    // A closed local port: a build that calls out fails at once, and the same way on both sides.
    ("HTTP_PROXY", CLOSED),
    ("HTTPS_PROXY", CLOSED),
    ("ALL_PROXY", CLOSED),
    ("http_proxy", CLOSED),
    ("https_proxy", CLOSED),
    ("all_proxy", CLOSED),
];
const CLOSED: &str = "http://127.0.0.1:9";

/// What a build needs from the caller to start at all; nothing else is inherited.
const INHERITED: [&str; 4] = ["PATH", "SystemRoot", "PATHEXT", "ComSpec"];

/// The token that stands for a side's own directory.
const DIR: &str = "<DIR>";

/// One build in its own directories: `work` holds the fixture, `home` and `tmp` start empty.
pub(super) struct Side<'a> {
    plan: &'a Plan,
    scenario: &'a Scenario,
    binary: &'a Path,
    base: PathBuf,
    own: Vec<Rule>,
}

/// A path as a program prints it: Windows adds a prefix to a resolved path that no program shows.
fn printed(path: &Path) -> String {
    let shown = path.to_string_lossy();
    shown.strip_prefix(r"\\?\").unwrap_or(&shown).to_string()
}

impl<'a> Side<'a> {
    pub(super) fn made(
        plan: &'a Plan,
        scenario: &'a Scenario,
        binary: &'a Path,
        base: PathBuf,
    ) -> Result<Self, String> {
        for dir in ["work", "home", "tmp"] {
            let made = std::fs::create_dir_all(base.join(dir));
            made.map_err(|e| format!("cannot make {}: {e}", base.display()))?;
        }
        if let Some(fixture) = &plan.fixture {
            copied(fixture, &base.join("work"))?;
        }
        let resolved = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        let own = vec![
            literal(printed(&resolved), DIR),
            literal(printed(&base), DIR),
        ];
        Ok(Self {
            plan,
            scenario,
            binary,
            base,
            own,
        })
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(self.binary);
        command
            .args(args)
            .current_dir(self.base.join("work"))
            .env_clear();
        for name in INHERITED {
            command.envs(std::env::var_os(name).map(|value| (name, value)));
        }
        for name in ["HOME", "USERPROFILE"] {
            command.env(name, self.base.join("home"));
        }
        for name in ["TMPDIR", "TEMP", "TMP"] {
            command.env(name, self.base.join("tmp"));
        }
        command.envs(FIXED).envs(&self.scenario.env);
        command
    }

    /// One command's answer. A build chock had to kill answers `timeout`; one that did not start
    /// is an error, since nothing was compared.
    fn answer(&self, args: &[String], stdin: &str) -> Result<Answer, String> {
        let name = self.binary.to_string_lossy();
        match fixed::run(&mut self.command(args), &name, stdin, self.plan.limit) {
            Ok(out) if out.truncated => Err(format!(
                "`{}` wrote more than chock keeps of one stream",
                args.join(" ")
            )),
            Ok(out) => Ok(Answer {
                exit: out
                    .code
                    .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                stdout: self.plan.normal(&out.stdout, &self.own),
                stderr: self.plan.normal(&out.stderr, &self.own),
            }),
            Err(stopped) if stopped.hung() => Ok(Answer {
                exit: TIMEOUT.to_string(),
                ..Answer::default()
            }),
            Err(stopped) => Err(stopped.to_string()),
        }
    }

    pub(super) fn ran(&self) -> Result<Ran, String> {
        let mut ran = Ran::default();
        for args in &self.scenario.steps {
            ran.steps.push(self.answer(args, &self.scenario.stdin)?);
        }
        for args in &self.plan.probes {
            ran.probes.push(self.answer(args, "")?);
        }
        let work = self.base.join("work");
        tree(&work, &work, &mut ran.tree)?;
        Ok(ran)
    }
}
