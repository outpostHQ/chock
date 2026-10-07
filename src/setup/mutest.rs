//! The mutation tool as `chock init --global` installs it and `chock doctor` judges it: Outpost's
//! fork of mutest-rs, built from the newest commit of its `main`.

use crate::exec::{self, Start};
use crate::gates::mutation::tool::{self, FORK, Tool};
use crate::run::report::Verdict;
use crate::setup::pins::Pin;
use std::path::Path;

/// The crate the pin file names, and the command of its row in `chock doctor`.
pub const CRATE: &str = "cargo-mutest";

const BRANCH: &str = "refs/heads/main";

/// No prompt for a password where the repository does not answer.
const NO_PROMPT: [(&str, &str); 1] = [("GIT_TERMINAL_PROMPT", "0")];

const REPAIR: &str = "run `chock init --global`";

/// Whether this pin is the fork, for a system that runs it: it is on no registry, so it has no
/// version to fetch.
#[must_use]
pub fn is_pin(pin: &Pin, os: &str) -> bool {
    pin.crate_name == CRATE && pin.unpublished() && pin.elsewhere(os).is_none()
}

/// Printed before the install, because a build takes minutes and silence reads as hung.
#[must_use]
pub fn starting() -> String {
    format!("  checking  {CRATE} — against `main` at {FORK}; a build of a new commit takes minutes")
}

/// Builds the fork at the newest commit of `main`, unless that build is here already. `Ok` and
/// `Err` both hold the line to print; `Err` is an install that failed.
pub fn install(start: Start, temp: &Path) -> Result<String, String> {
    // Named for this process, so two installs at once do not build in one folder.
    let scratch = temp.join(format!("chock-{}", std::process::id()));
    let done = newest(start, temp).and_then(|commit| {
        let clone = scratch.join(format!("mutest-rs-{commit}"));
        cloned(start, &clone, &commit).and_then(|()| built(start, &clone, &commit))
    });
    // Deleted in each case; a folder that stays costs disk, not a wrong tool.
    let _ = std::fs::remove_dir_all(&scratch);
    done.map_err(|why| format!("  FAILED    {CRATE} — {why}"))
}

/// The commit `main` of the fork is at now.
fn newest(start: Start, cwd: &Path) -> Result<String, String> {
    let said = exec::printed(start, "git", &["ls-remote", FORK, BRANCH], cwd, &NO_PROMPT)?;
    commit_in(&said).ok_or_else(|| format!("{FORK} has no `main` branch to build from"))
}

/// The commit of a `git ls-remote` line: its 40 hex digits, a tab, then the name of the branch.
fn commit_in(said: &str) -> Option<String> {
    let (commit, name) = said.lines().next()?.split_once('\t')?;
    (name == BRANCH && is_commit(commit)).then(|| commit.to_string())
}

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The fork at `commit` in `clone`. Cargo records this path, so its name carries the commit.
fn cloned(start: Start, clone: &Path, commit: &str) -> Result<(), String> {
    // An install that was stopped under this process id may have left one.
    let _ = std::fs::remove_dir_all(clone);
    std::fs::create_dir_all(clone)
        .map_err(|e| format!("cannot create {}: {e}", clone.display()))?;
    let steps: [&[&str]; 3] = [
        &["init", "--quiet"],
        &["fetch", "--quiet", "--depth", "1", FORK, commit],
        &["checkout", "--quiet", "FETCH_HEAD"],
    ];
    steps
        .iter()
        .try_for_each(|args| exec::printed(start, "git", args, clone, &NO_PROMPT).map(drop))
}

/// The build in `clone`, or no build where cargo's record and the tool itself say it is here.
fn built(start: Start, clone: &Path, commit: &str) -> Result<String, String> {
    let toolchain = toolchain_in(clone)?;
    install_toolchain(start, clone, &toolchain)?;
    if current(start, clone, commit)? {
        return Ok(format!(
            "  current   {CRATE} {} — built from the newest commit of `main`",
            short(commit)
        ));
    }
    build(start, clone, &toolchain.channel)?;
    tool::asked(start, clone)
        .and_then(tool::refusal)
        .map_err(|why| {
            format!("the build finished, but {why}; is another `{CRATE}` earlier on `PATH`?")
        })?;
    Ok(format!(
        "  installed {CRATE} {} — the newest commit of `main`",
        short(commit)
    ))
}

/// The toolchain the fork builds with, as its `rust-toolchain.toml` names it.
#[derive(Debug, PartialEq, Eq)]
struct Toolchain {
    channel: String,
    components: Vec<String>,
}

fn toolchain_in(clone: &Path) -> Result<Toolchain, String> {
    let file = clone.join("rust-toolchain.toml");
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    read_toolchain(&text)
        .ok_or_else(|| format!("{} names no toolchain chock can read", file.display()))
}

fn read_toolchain(text: &str) -> Option<Toolchain> {
    let channel = quoted(text, "channel").into_iter().next()?;
    let components = quoted(text, "components");
    let named = std::iter::once(&channel)
        .chain(&components)
        .all(|name| plain(name));
    named.then_some(Toolchain {
        channel,
        components,
    })
}

/// The quoted values on the line that sets `key`. A list that continues on the next line is not
/// read.
fn quoted(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(key)?
                .trim_start()
                .strip_prefix('=')
        })
        .flat_map(|value| value.split('"').skip(1).step_by(2))
        .map(str::to_string)
        .collect()
}

/// A toolchain or component name and nothing else, since it becomes an argument to rustup.
fn plain(name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    name.starts_with(|c: char| c.is_ascii_alphanumeric()) && name.chars().all(word)
}

/// The toolchain, then each part the fork links against; rustup skips what is here already.
fn install_toolchain(start: Start, clone: &Path, toolchain: &Toolchain) -> Result<(), String> {
    let channel = toolchain.channel.as_str();
    let install = [
        "toolchain",
        "install",
        channel,
        "--profile",
        "minimal",
        "--no-self-update",
    ];
    exec::printed(start, "rustup", &install, clone, &[])?;
    toolchain.components.iter().try_for_each(|part| {
        let add = ["component", "add", "--toolchain", channel, part];
        exec::printed(start, "rustup", &add, clone, &[]).map(drop)
    })
}

/// Whether the installed tool is the build of `commit`: cargo's record names it, and it answers.
fn current(start: Start, cwd: &Path, commit: &str) -> Result<bool, String> {
    let listing = exec::printed(start, "cargo", &["install", "--list"], cwd, &[])?;
    Ok(recorded(&listing) == Some(commit) && tool::asked(start, cwd)? == Tool::Usable)
}

/// The commit cargo's record says both programs were built from; `None` for any other build.
#[must_use]
pub fn recorded(listing: &str) -> Option<&str> {
    let [tool, driver] =
        ["cargo-mutest v", "mutest-driver v"].map(|name| source_commit(listing, name));
    tool.filter(|_| tool == driver)
}

/// The commit in the path of a `cargo install --list` line, where chock's clone was the source.
fn source_commit<'a>(listing: &'a str, name: &str) -> Option<&'a str> {
    let line = listing.lines().find(|line| line.starts_with(name))?;
    line.split(['/', '\\'])
        .filter_map(|part| part.strip_prefix("mutest-rs-"))
        .find(|rest| is_commit(rest))
}

/// The fork's own install steps: the arguments of each `cargo` call, in order.
const STEPS: [&str; 3] = [
    "build --quiet --locked --profile=release -p mutest-runtime",
    "install --quiet --locked --force --path mutest-driver",
    "install --quiet --locked --force --path cargo-mutest",
];

/// The empty `CARGO_ENCODED_RUSTFLAGS` keeps a caller's `RUSTFLAGS` out of the build; the flags
/// the fork needs are in its manifest.
fn build(start: Start, clone: &Path, channel: &str) -> Result<(), String> {
    let env = [
        ("RUSTUP_TOOLCHAIN", channel),
        ("CARGO_ENCODED_RUSTFLAGS", ""),
    ];
    STEPS.iter().try_for_each(|step| {
        let args: Vec<&str> = step.split(' ').collect();
        exec::printed(start, "cargo", &args, clone, &env).map(drop)
    })
}

/// How the installed tool stands against the fork's `main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    Current(String),
    Behind {
        have: String,
        newest: String,
    },
    /// Usable, with no commit to compare: a build chock did not make, or no answer from `main`.
    Unknown(String),
    Foreign,
    Absent,
    /// Cargo could not be asked which tool it has.
    Unasked(String),
}

/// Asks the tool, then cargo's record, then the fork; each next question only where it matters.
pub fn standing(start: Start, cwd: &Path) -> Standing {
    match tool::asked(start, cwd) {
        Ok(Tool::Usable) => compared(start, cwd),
        Ok(Tool::Foreign) => Standing::Foreign,
        Ok(Tool::Absent) => Standing::Absent,
        Err(why) => Standing::Unasked(why),
    }
}

fn compared(start: Start, cwd: &Path) -> Standing {
    let listing = exec::printed(start, "cargo", &["install", "--list"], cwd, &[]);
    let Some(have) = listing.as_deref().ok().and_then(recorded) else {
        let why = "a build chock did not make, so it cannot tell its commit";
        return Standing::Unknown(why.to_string());
    };
    let have = have.to_string();
    match newest(start, cwd) {
        Ok(newest) if newest == have => Standing::Current(have),
        Ok(newest) => Standing::Behind { have, newest },
        Err(why) => Standing::Unknown(format!(
            "built from {}; chock could not ask for the newest commit of `main`: {}",
            short(&have),
            why.lines().next().unwrap_or_default()
        )),
    }
}

impl Standing {
    /// The verdict and the words of the row `chock doctor` prints.
    #[must_use]
    pub fn row(&self) -> (Verdict, String) {
        match self {
            Self::Current(have) => (
                Verdict::Pass,
                format!("built from {}, the newest commit of `main`", short(have)),
            ),
            Self::Behind { have, newest } => (
                Verdict::Tripped,
                format!(
                    "built from {}; `main` is at {}: {REPAIR}",
                    short(have),
                    short(newest)
                ),
            ),
            Self::Unknown(why) => (Verdict::Pass, why.clone()),
            Self::Foreign => (
                Verdict::Tripped,
                format!("not a current build of Outpost's fork: {REPAIR}"),
            ),
            Self::Absent => (Verdict::Tripped, format!("not installed: {REPAIR}")),
            Self::Unasked(why) => (Verdict::CannotRun, why.clone()),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const NEWEST: &str = "392a07841b5f05acc96a62facac7d24e9a8c3182";
    const OLDER: &str = "0123456789abcdef0123456789abcdef01234567";
    const FORK_TOOLCHAIN: &str = "[toolchain]\nchannel = \"nightly-2026-10-03\"\ncomponents = [\"rustc-dev\", \"llvm-tools\"]\n";
    const HELP: &str = "      --require-progress  Require protocol-v1 progress\n";
    /// An exit code that stands for a program that did not start.
    const NO_START: i32 = i32::MIN;

    /// A machine with fixed answers. The first rule whose text a command holds answers it with an
    /// exit code and what it printed; a command no rule holds passes and prints nothing.
    struct Machine<'a> {
        rules: &'a [(&'a str, i32, &'a str)],
        /// What `git checkout` leaves as `rust-toolchain.toml`.
        toolchain: Option<&'a str>,
        log: RefCell<Vec<String>>,
    }

    impl<'a> Machine<'a> {
        fn new(rules: &'a [(&'a str, i32, &'a str)]) -> Self {
            Self {
                rules,
                toolchain: Some(FORK_TOOLCHAIN),
                log: RefCell::new(Vec::new()),
            }
        }

        fn run(
            &self,
            program: &str,
            args: &[&str],
            cwd: &Path,
            env: &[(&str, &str)],
        ) -> Result<exec::Output, exec::ExecError> {
            let set: String = env
                .iter()
                .map(|(key, value)| format!("{key}={value} "))
                .collect();
            let line = format!("{set}{program} {}", args.join(" "));
            if let (true, Some(text)) = (line.contains("git checkout"), self.toolchain) {
                std::fs::write(cwd.join("rust-toolchain.toml"), text).unwrap();
            }
            self.log.borrow_mut().push(line.clone());
            let rule = self.rules.iter().find(|(text, ..)| line.contains(text));
            match rule.map_or((0, ""), |(_, code, said)| (*code, *said)) {
                (NO_START, reason) => Err(exec::ExecError {
                    program: program.to_string(),
                    stage: exec::Stage::Spawn,
                    reason: reason.to_string(),
                }),
                (0, said) => Ok(output(0, said, "")),
                (code, said) => Ok(output(code, "", said)),
            }
        }

        fn log(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }

    fn branch(commit: &str) -> String {
        format!("{commit}\trefs/heads/main\n")
    }

    /// Where an install under `temp` clones `NEWEST`, joined part by part as `install` joins it.
    fn clone_under(temp: &Path) -> std::path::PathBuf {
        let scratch = temp.join(format!("chock-{}", std::process::id()));
        scratch.join(format!("mutest-rs-{NEWEST}"))
    }

    fn records(commit: &str) -> String {
        format!(
            "cargo-deny v0.20.2:\n    cargo-deny\n\
             cargo-mutest v0.0.0 (/tmp/chock-7/mutest-rs-{commit}/cargo-mutest):\n    cargo-mutest\n\
             mutest-driver v0.0.0 (/tmp/chock-7/mutest-rs-{commit}/mutest-driver):\n    mutest-driver\n"
        )
    }

    fn installing(machine: &Machine, temp: &Path) -> Result<String, String> {
        install(
            &|program, args, cwd, env| machine.run(program, args, cwd, env),
            temp,
        )
    }

    fn judged(machine: &Machine) -> Standing {
        let start: Start = &|program, args, cwd, env| machine.run(program, args, cwd, env);
        standing(start, Path::new("."))
    }

    fn pin(crate_name: &str, want: &str) -> Pin {
        Pin {
            key: String::new(),
            crate_name: crate_name.to_string(),
            command: crate_name.to_string(),
            want: want.to_string(),
            setup: None,
            systems: None,
        }
    }

    const GIT: &str = "GIT_TERMINAL_PROMPT=0 git";
    const CARGO: &str = "RUSTUP_TOOLCHAIN=nightly-2026-10-03 CARGO_ENCODED_RUSTFLAGS= cargo";

    /// Every command of an install that reaches the build, in order.
    fn whole_install() -> Vec<String> {
        vec![
            format!("{GIT} ls-remote {FORK} refs/heads/main"),
            format!("{GIT} init --quiet"),
            format!("{GIT} fetch --quiet --depth 1 {FORK} {NEWEST}"),
            format!("{GIT} checkout --quiet FETCH_HEAD"),
            "rustup toolchain install nightly-2026-10-03 --profile minimal --no-self-update"
                .to_string(),
            "rustup component add --toolchain nightly-2026-10-03 rustc-dev".to_string(),
            "rustup component add --toolchain nightly-2026-10-03 llvm-tools".to_string(),
            "cargo install --list".to_string(),
            format!("{CARGO} build --quiet --locked --profile=release -p mutest-runtime"),
            format!("{CARGO} install --quiet --locked --force --path mutest-driver"),
            format!("{CARGO} install --quiet --locked --force --path cargo-mutest"),
            "cargo mutest run --help".to_string(),
        ]
    }

    #[test]
    fn only_the_unpublished_pin_of_cargo_mutest_is_the_fork() {
        assert!(is_pin(&pin("cargo-mutest", "0.0.0"), "macos"));
        assert!(!is_pin(&pin("cargo-mutest", "1.2.3"), "macos"));
        assert!(!is_pin(&pin("outpost", "0.0.0"), "macos"));
        let linux_only = Pin {
            systems: Some(vec!["linux".to_string()]),
            ..pin("cargo-mutest", "0.0.0")
        };
        assert!(is_pin(&linux_only, "linux"));
        assert!(!is_pin(&linux_only, "macos"), "init skips it there");
        assert_eq!(
            starting(),
            "  checking  cargo-mutest — against `main` at https://github.com/outpostHQ/mutest-rs; a build of a new commit takes minutes"
        );
    }

    #[test]
    fn the_newest_commit_is_the_40_hex_digits_git_prints_for_main() {
        assert_eq!(commit_in(&branch(NEWEST)), Some(NEWEST.to_string()));
        assert_eq!(commit_in(""), None);
        assert_eq!(commit_in(NEWEST), None);
        assert_eq!(commit_in(&format!("{NEWEST}\trefs/heads/main2\n")), None);
        let (short_one, long_one) = (&NEWEST[1..], format!("{NEWEST}0"));
        assert_eq!(commit_in(&branch(short_one)), None);
        assert_eq!(commit_in(&branch(&long_one)), None);
        assert_eq!(commit_in(&branch(&NEWEST.replace('3', "g"))), None);
        assert_eq!([short(NEWEST), short("392a")], ["392a078", "392a"]);
    }

    #[test]
    fn the_toolchain_is_what_the_file_of_the_fork_names_and_nothing_chock_cannot_pass_on() {
        let fork = Toolchain {
            channel: "nightly-2026-10-03".to_string(),
            components: vec!["rustc-dev".to_string(), "llvm-tools".to_string()],
        };
        assert_eq!(read_toolchain(FORK_TOOLCHAIN), Some(fork));
        let bare = Toolchain {
            channel: "1.95.0".to_string(),
            components: Vec::new(),
        };
        // A list that continues on the next line is not read, and another key is not `channel`.
        let text = "channel_of = \"x\"\n  channel=\"1.95.0\"\ncomponents = [\n  \"miri\",\n]\n";
        assert_eq!(read_toolchain(text), Some(bare));
        assert_eq!(
            read_toolchain("[toolchain]\ncomponents = [\"miri\"]\n"),
            None
        );
        assert_eq!(read_toolchain("channel = \"nightly; rm\"\n"), None);
        assert_eq!(read_toolchain("channel = \"-nightly\"\n"), None);
        assert_eq!(read_toolchain("channel = \"\"\n"), None);
        let flag = "channel = \"nightly\"\ncomponents = [\"miri\", \"--force\"]\n";
        assert_eq!(read_toolchain(flag), None);
    }

    #[test]
    fn the_recorded_commit_is_the_one_both_programs_were_built_from() {
        assert_eq!(recorded(&records(NEWEST)), Some(NEWEST));
        let windows = records(NEWEST).replace('/', "\\");
        assert_eq!(recorded(&windows), Some(NEWEST));
        let by_hand = "cargo-mutest v0.0.0 (/home/u/mutest-rs/cargo-mutest):\n    cargo-mutest\n\
                       mutest-driver v0.0.0 (/home/u/mutest-rs/mutest-driver):\n    mutest-driver\n";
        assert_eq!(recorded(by_hand), None);
        let mixed = records(NEWEST).replacen(NEWEST, OLDER, 1);
        assert_eq!(recorded(&mixed), None);
        let no_driver = records(NEWEST).replace("mutest-driver v", "other-driver v");
        assert_eq!(recorded(&no_driver), None);
        assert_eq!(recorded(""), None);
    }

    #[test]
    fn a_machine_without_the_build_gets_the_fork_built_at_the_newest_commit() {
        let temp = crate::testdir::make("mutest-fresh");
        let newest = branch(NEWEST);
        let rules = [("ls-remote", 0, newest.as_str()), ("run --help", 0, HELP)];
        let machine = Machine::new(&rules);
        assert_eq!(
            installing(&machine, &temp),
            Ok("  installed cargo-mutest 392a078 — the newest commit of `main`".to_string())
        );
        assert_eq!(machine.log(), whole_install());
        let left: Vec<_> = std::fs::read_dir(&*temp).unwrap().collect();
        assert!(left.is_empty(), "the clone is deleted: {left:?}");
    }

    #[test]
    fn a_machine_with_the_build_of_the_newest_commit_builds_nothing() {
        let temp = crate::testdir::make("mutest-current");
        let (newest, listing) = (branch(NEWEST), records(NEWEST));
        let rules = [
            ("ls-remote", 0, newest.as_str()),
            ("install --list", 0, listing.as_str()),
            ("run --help", 0, HELP),
        ];
        let machine = Machine::new(&rules);
        assert_eq!(
            installing(&machine, &temp),
            Ok(
                "  current   cargo-mutest 392a078 — built from the newest commit of `main`"
                    .to_string()
            )
        );
        let mut asked = whole_install();
        asked.drain(8..11);
        assert_eq!(machine.log(), asked);
    }

    #[test]
    fn an_older_build_and_a_recorded_build_that_does_not_answer_are_both_built_again() {
        let newest = branch(NEWEST);
        for listing in [records(OLDER), records(NEWEST)] {
            let temp = crate::testdir::make("mutest-again");
            // The tool takes the flag only after the build, when the older record is the case.
            let help = if listing == records(OLDER) {
                HELP
            } else {
                "Options:\n"
            };
            let rules = [
                ("ls-remote", 0, newest.as_str()),
                ("install --list", 0, listing.as_str()),
                ("run --help", 0, help),
            ];
            let machine = Machine::new(&rules);
            let done = installing(&machine, &temp);
            let built = machine
                .log()
                .iter()
                .filter(|line| line.starts_with(CARGO))
                .count();
            assert_eq!(built, 3, "{listing}");
            let foreign = format!(
                "  FAILED    cargo-mutest — the build finished, but {}; is another `cargo-mutest` earlier on `PATH`?",
                tool::FOREIGN
            );
            let expected = match help == HELP {
                true => Ok(
                    "  installed cargo-mutest 392a078 — the newest commit of `main`".to_string(),
                ),
                false => Err(foreign),
            };
            assert_eq!(done, expected);
        }
    }

    #[test]
    fn each_step_that_fails_ends_the_install_with_its_command_and_deletes_the_clone() {
        let newest = branch(NEWEST);
        let stderr = "stderr (bounded output):\nerror: no\n";
        for (failing, command, ran) in [
            (
                "ls-remote",
                format!("git ls-remote {FORK} refs/heads/main"),
                1,
            ),
            (
                "git fetch",
                format!("git fetch --quiet --depth 1 {FORK} {NEWEST}"),
                3,
            ),
            (
                "toolchain install",
                "rustup toolchain install nightly-2026-10-03 --profile minimal --no-self-update"
                    .to_string(),
                5,
            ),
            (
                "llvm-tools",
                "rustup component add --toolchain nightly-2026-10-03 llvm-tools".to_string(),
                7,
            ),
            ("install --list", "cargo install --list".to_string(), 8),
            (
                "--path mutest-driver",
                "cargo install --quiet --locked --force --path mutest-driver".to_string(),
                10,
            ),
        ] {
            let temp = crate::testdir::make("mutest-step");
            let rules = [
                (failing, 101, "error: no\n"),
                ("ls-remote", 0, newest.as_str()),
            ];
            let machine = Machine::new(&rules);
            assert_eq!(
                installing(&machine, &temp),
                Err(format!(
                    "  FAILED    cargo-mutest — `{command}` failed: error: no\n{stderr}"
                ))
            );
            assert_eq!(machine.log(), whole_install()[..ran], "{failing}");
            let left: Vec<_> = std::fs::read_dir(&*temp).unwrap().collect();
            assert!(left.is_empty(), "{failing}: {left:?}");
        }
    }

    #[test]
    fn a_fork_with_no_main_a_clone_with_no_toolchain_and_a_cargo_that_cannot_answer_each_fail() {
        let temp = crate::testdir::make("mutest-odd");
        let newest = branch(NEWEST);
        let clone = clone_under(&temp);
        let file = clone.join("rust-toolchain.toml");

        let no_branch = Machine::new(&[]);
        assert_eq!(
            installing(&no_branch, &temp),
            Err(format!(
                "  FAILED    cargo-mutest — {FORK} has no `main` branch to build from"
            ))
        );

        let rules = [("ls-remote", 0, newest.as_str())];
        let no_file = Machine {
            toolchain: None,
            ..Machine::new(&rules)
        };
        let failed = installing(&no_file, &temp).unwrap_err();
        let cannot_read = format!(
            "  FAILED    cargo-mutest — cannot read {}: ",
            file.display()
        );
        assert!(failed.starts_with(&cannot_read), "{failed}");

        let unreadable = Machine {
            toolchain: Some("[toolchain]\n"),
            ..Machine::new(&rules)
        };
        assert_eq!(
            installing(&unreadable, &temp),
            Err(format!(
                "  FAILED    cargo-mutest — {} names no toolchain chock can read",
                file.display()
            ))
        );

        let rules = [
            ("ls-remote", 0, newest.as_str()),
            ("run --help", NO_START, "not found"),
        ];
        let mute = Machine::new(&rules);
        assert_eq!(
            installing(&mute, &temp),
            Err("  FAILED    cargo-mutest — the build finished, but could not start cargo: not found; is another `cargo-mutest` earlier on `PATH`?".to_string())
        );

        // A file where the scratch folder goes, so the clone cannot be created.
        let blocked = temp.join("a-file");
        std::fs::write(&blocked, "").unwrap();
        let failed = installing(&Machine::new(&rules), &blocked).unwrap_err();
        let clone = clone_under(&blocked);
        let cannot_create = format!(
            "  FAILED    cargo-mutest — cannot create {}: ",
            clone.display()
        );
        assert!(failed.starts_with(&cannot_create), "{failed}");
    }

    #[test]
    fn a_recorded_build_that_stops_answering_between_the_two_questions_is_an_error() {
        let temp = crate::testdir::make("mutest-mute");
        let (newest, listing) = (branch(NEWEST), records(NEWEST));
        let rules = [
            ("ls-remote", 0, newest.as_str()),
            ("install --list", 0, listing.as_str()),
            ("run --help", NO_START, "not found"),
        ];
        assert_eq!(
            installing(&Machine::new(&rules), &temp),
            Err("  FAILED    cargo-mutest — could not start cargo: not found".to_string())
        );
    }

    #[test]
    fn the_standing_of_a_tool_that_cannot_be_driven_needs_no_second_question() {
        let absent = [("run --help", 101, "error: no such command: `mutest`\n")];
        let foreign = [("run --help", 0, "Options:\n")];
        let mute = [("run --help", NO_START, "not found")];
        for (rules, standing, row) in [
            (
                &absent,
                Standing::Absent,
                (Verdict::Tripped, "not installed: run `chock init --global`"),
            ),
            (
                &foreign,
                Standing::Foreign,
                (
                    Verdict::Tripped,
                    "not a current build of Outpost's fork: run `chock init --global`",
                ),
            ),
            (
                &mute,
                Standing::Unasked("could not start cargo: not found".to_string()),
                (Verdict::CannotRun, "could not start cargo: not found"),
            ),
        ] {
            let machine = Machine::new(rules);
            let found = judged(&machine);
            assert_eq!(found, standing);
            assert_eq!(found.row(), (row.0, row.1.to_string()));
            assert_eq!(machine.log(), ["cargo mutest run --help"]);
        }
    }

    #[test]
    fn a_usable_tool_stands_by_its_recorded_commit_against_the_newest_one() {
        let (newest, same, older) = (branch(NEWEST), records(NEWEST), records(OLDER));
        let current = [
            ("run --help", 0, HELP),
            ("install --list", 0, same.as_str()),
            ("ls-remote", 0, newest.as_str()),
        ];
        let found = judged(&Machine::new(&current));
        assert_eq!(found, Standing::Current(NEWEST.to_string()));
        assert_eq!(
            found.row(),
            (
                Verdict::Pass,
                "built from 392a078, the newest commit of `main`".to_string()
            )
        );

        let behind = [
            ("run --help", 0, HELP),
            ("install --list", 0, older.as_str()),
            ("ls-remote", 0, newest.as_str()),
        ];
        let found = judged(&Machine::new(&behind));
        let expected = Standing::Behind {
            have: OLDER.to_string(),
            newest: NEWEST.to_string(),
        };
        assert_eq!(found, expected);
        assert_eq!(
            found.row(),
            (
                Verdict::Tripped,
                "built from 0123456; `main` is at 392a078: run `chock init --global`".to_string()
            )
        );
    }

    #[test]
    fn a_usable_tool_with_no_commit_to_compare_passes_and_says_why() {
        let by_hand = "a build chock did not make, so it cannot tell its commit";
        for rules in [
            [("run --help", 0, HELP), ("install --list", 0, "")],
            [
                ("run --help", 0, HELP),
                ("install --list", 101, "error: no\n"),
            ],
        ] {
            let machine = Machine::new(&rules);
            let found = judged(&machine);
            assert_eq!(found, Standing::Unknown(by_hand.to_string()));
            assert_eq!(found.row(), (Verdict::Pass, by_hand.to_string()));
            assert_eq!(
                machine.log(),
                ["cargo mutest run --help", "cargo install --list"]
            );
        }

        let older = records(OLDER);
        let offline = [
            ("run --help", 0, HELP),
            ("install --list", 0, older.as_str()),
            ("ls-remote", 128, "fatal: unable to access the fork\n"),
        ];
        let why = format!(
            "built from 0123456; chock could not ask for the newest commit of `main`: `git ls-remote {FORK} refs/heads/main` failed: fatal: unable to access the fork"
        );
        assert_eq!(judged(&Machine::new(&offline)), Standing::Unknown(why));
    }
}
