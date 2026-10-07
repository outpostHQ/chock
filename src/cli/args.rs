//! The command line as a value: each word and flag parsed, and nothing run.

use super::Change;
use crate::gates::tools::miri::Part;
use crate::project::config::Stage;
use crate::usage::{USAGE, VERSION_LINE};

/// The parsed command line, kept apart from dispatch so a test can check it without a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command<'a> {
    Run {
        names: Vec<&'a str>,
        json: bool,
        fast: bool,
        /// Set in CI, where checks whose tool CI cannot install are left out.
        ci: bool,
        /// Judges every gate again, with no kept verdict recalled.
        no_cache: bool,
        /// The part of the Miri suite to run, from `--miri-partition=K/N`.
        miri_part: Option<Part>,
    },
    /// `cache clear`: removes every kept verdict.
    CacheClear,
    Gates {
        json: bool,
    },
    /// `enable`, `disable` and `stage`: one change to each named gate in the config.
    Configure {
        names: Vec<&'a str>,
        change: Change<'a>,
    },
    /// One gate's last findings, or with no gate all the debt on record.
    Explain {
        name: Option<&'a str>,
        json: bool,
    },
    Baseline(Vec<&'a str>),
    Doctor {
        json: bool,
    },
    /// Measures a tree that chock does not own: no config, no baseline, no writes.
    Survey {
        json: bool,
    },
    Message(&'a str),
    /// What a git hook runs. The body lives in chock, so an upgrade updates every hook.
    Hook(&'a [&'a str]),
    Edited(&'a [&'a str]),
    Slop(&'a [&'a str]),
    Init(&'a [&'a str]),
    /// `--version` and `--help`: the text each one prints.
    Print(&'static str),
    Usage(String),
}

/// `--help` or `-h` anywhere after a command; `init` answers it with its own text.
fn asks_for_help(rest: &[&str]) -> bool {
    rest.iter().any(|arg| matches!(*arg, "--help" | "-h"))
}

/// Accepts `--json` anywhere after the subcommand.
pub(super) fn split_json<'a>(args: &[&'a str]) -> (Vec<&'a str>, bool) {
    let json = args.contains(&"--json");
    (
        args.iter().copied().filter(|a| *a != "--json").collect(),
        json,
    )
}

/// A command whose one option is `--json`, and which refuses any other argument.
fn json_only<'a>(cmd: &str, rest: &[&str], made: impl FnOnce(bool) -> Command<'a>) -> Command<'a> {
    match split_json(rest) {
        (extra, json) if extra.is_empty() => made(json),
        (extra, _) => Command::Usage(format!("{cmd} takes no argument `{}`", extra[0])),
    }
}

/// The change for the named gates, or a usage error when the command names none.
fn configures<'a>(cmd: &str, names: Vec<&'a str>, change: Change<'a>) -> Command<'a> {
    if names.is_empty() {
        return Command::Usage(format!("{cmd} needs at least one gate name"));
    }
    Command::Configure { names, change }
}

const PARTITION: &str = "--miri-partition";

/// A flag of `run`: `--fast`, `--ci`, `--no-cache` or `--miri-partition=K/N`.
fn known(flag: &str) -> bool {
    matches!(flag, "--fast" | "--ci" | "--no-cache") || flag.starts_with(PARTITION)
}

/// `run`: the gates it names, with its flags and `--json` wherever they stand.
fn to_run<'a>(rest: &[&'a str]) -> Command<'a> {
    let (names, json) = split_json(rest);
    match names.iter().find(|n| n.starts_with('-') && !known(n)) {
        Some(flag) => Command::Usage(format!("unknown option `{flag}`")),
        // A malformed `--miri-partition` is a usage error too.
        None => miri_part(&names).map_or_else(Command::Usage, |part| Command::Run {
            fast: names.contains(&"--fast"),
            ci: names.contains(&"--ci"),
            no_cache: names.contains(&"--no-cache"),
            names: names.into_iter().filter(|n| !known(n)).collect(),
            json,
            miri_part: part,
        }),
    }
}

/// The part `--miri-partition=K/N` names; the flag given twice is refused.
fn miri_part(names: &[&str]) -> Result<Option<Part>, String> {
    let mut asked = names.iter().filter_map(|n| n.strip_prefix(PARTITION));
    match (asked.next(), asked.next()) {
        (_, Some(_)) => Err(format!("`{PARTITION}` is given twice")),
        (first, None) => first
            .map(|value| Part::parse(value.strip_prefix('=').unwrap_or(value)))
            .transpose(),
    }
}

#[must_use]
pub fn parse<'a>(args: &'a [&'a str]) -> Command<'a> {
    match args {
        [cmd, rest @ ..] if *cmd != "init" && asks_for_help(rest) => Command::Print(USAGE),
        ["run", rest @ ..] => to_run(rest),
        ["gates", rest @ ..] => json_only("gates", rest, |json| Command::Gates { json }),
        ["enable", rest @ ..] => configures("enable", rest.to_vec(), Change::On),
        ["disable", rest @ ..] => switched_off(rest),
        ["stage", rest @ ..] => restaged(rest),
        ["message", file] => Command::Message(file),
        ["hook", rest @ ..] => Command::Hook(rest),
        ["message", ..] => Command::Usage("message takes exactly one file".to_string()),
        ["explain", rest @ ..] => explained(rest),
        ["baseline", rest @ ..] => Command::Baseline(rest.to_vec()),
        ["cache", rest @ ..] => cached(rest),
        ["doctor", rest @ ..] => json_only("doctor", rest, |json| Command::Doctor { json }),
        ["survey", rest @ ..] => json_only("survey", rest, |json| Command::Survey { json }),
        ["edited", rest @ ..] => Command::Edited(rest),
        ["slop", rest @ ..] => Command::Slop(rest),
        ["init", rest @ ..] => Command::Init(rest),
        ["--version" | "-V"] => Command::Print(VERSION_LINE),
        ["help", "init"] => Command::Init(&["--help"]),
        ["--help" | "-h"] | ["help", ..] => Command::Print(USAGE),
        [] => Command::Usage("no command given".to_string()),
        [cmd, ..] => Command::Usage(format!("unknown command `{cmd}`")),
    }
}

/// `cache` takes one word today.
fn cached<'a>(rest: &[&str]) -> Command<'a> {
    match rest {
        ["clear"] => Command::CacheClear,
        _ => Command::Usage("cache takes one word: clear".to_string()),
    }
}

/// `disable`'s gates, and the `--reason` that the config records in place of the default reason.
fn switched_off<'a>(rest: &'a [&'a str]) -> Command<'a> {
    let mut names = Vec::new();
    let mut why = None;
    let mut args = rest.iter().copied();
    while let Some(arg) = args.next() {
        match arg {
            "--reason" => match args.next() {
                Some(text) if !text.trim().is_empty() => why = Some(text),
                _ => return Command::Usage("--reason needs the reason after it".to_string()),
            },
            flag if flag.starts_with('-') => {
                return Command::Usage(format!("unknown option `{flag}`"));
            }
            name => names.push(name),
        }
    }
    configures("disable", names, Change::Off(why))
}

/// `stage`'s gates, then the stage they move to.
fn restaged<'a>(rest: &'a [&'a str]) -> Command<'a> {
    match rest.split_last() {
        Some((last, names)) if !names.is_empty() => match Stage::named(last) {
            Some(stage) => Command::Configure {
                names: names.to_vec(),
                change: Change::Stage(stage),
            },
            None => Command::Usage(format!(
                "`{last}` is not a stage; the stages are commit, push, ci and manual"
            )),
        },
        _ => Command::Usage("stage needs gate names, then commit, push, ci or manual".to_string()),
    }
}

fn explained<'a>(rest: &'a [&'a str]) -> Command<'a> {
    match split_json(rest) {
        (names, json) if names.len() < 2 => Command::Explain {
            name: names.first().copied(),
            json,
        },
        _ => Command::Usage("explain takes one gate, or none for all the debt".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_subcommand_parses_to_its_own_action() {
        assert_eq!(parse(&["doctor"]), Command::Doctor { json: false });
        assert_eq!(parse(&["baseline"]), Command::Baseline(vec![]));
        assert_eq!(parse(&["gates"]), Command::Gates { json: false });
        assert_eq!(parse(&["--version"]), Command::Print(VERSION_LINE));
        assert_eq!(parse(&["-V"]), Command::Print(VERSION_LINE));
        assert_eq!(parse(&["--help"]), Command::Print(USAGE));
        assert_eq!(parse(&["-h"]), Command::Print(USAGE));
        assert_eq!(parse(&["help"]), Command::Print(USAGE));
        assert_eq!(parse(&["help", "run"]), Command::Print(USAGE));
        assert_eq!(parse(&["help", "init"]), Command::Init(&["--help"]));
    }

    #[test]
    fn a_subcommand_keeps_the_arguments_after_it() {
        assert_eq!(parse(&["init", "--global"]), Command::Init(&["--global"]));
        assert_eq!(parse(&["slop", "src"]), Command::Slop(&["src"]));
        assert_eq!(parse(&["init"]), Command::Init(&[]));
        assert_eq!(parse(&["slop"]), Command::Slop(&[]));
    }

    #[test]
    fn run_with_no_name_means_every_enforced_gate() {
        assert_eq!(
            parse(&["run"]),
            Command::Run {
                names: vec![],
                json: false,
                fast: false,
                ci: false,
                no_cache: false,
                miri_part: None
            }
        );
    }

    /// A commit hook cannot list gate names, because the enabled set changes.
    #[test]
    fn fast_is_a_flag_on_run_rather_than_a_list_of_names() {
        assert_eq!(
            parse(&["run", "--fast"]),
            Command::Run {
                names: vec![],
                json: false,
                fast: true,
                ci: false,
                no_cache: false,
                miri_part: None
            }
        );
        assert_eq!(
            parse(&["run", "--fast", "--json"]),
            Command::Run {
                names: vec![],
                json: true,
                fast: true,
                ci: false,
                no_cache: false,
                miri_part: None
            }
        );
    }

    #[test]
    fn run_keeps_the_gate_names_it_was_given_in_order() {
        assert_eq!(
            parse(&["run", "lint", "slop"]),
            Command::Run {
                names: vec!["lint", "slop"],
                json: false,
                fast: false,
                ci: false,
                no_cache: false,
                miri_part: None
            }
        );
    }

    #[test]
    fn the_json_flag_is_accepted_before_or_after_the_gate_names() {
        let expected = Command::Run {
            names: vec!["lint"],
            json: true,
            fast: false,
            ci: false,
            no_cache: false,
            miri_part: None,
        };
        assert_eq!(parse(&["run", "--json", "lint"]), expected);
        assert_eq!(parse(&["run", "lint", "--json"]), expected);
    }

    #[test]
    fn no_cache_is_a_flag_on_run_wherever_it_stands() {
        let expected = Command::Run {
            names: vec!["lint"],
            json: false,
            fast: false,
            ci: true,
            no_cache: true,
            miri_part: None,
        };
        assert_eq!(parse(&["run", "--no-cache", "--ci", "lint"]), expected);
        assert_eq!(parse(&["run", "lint", "--ci", "--no-cache"]), expected);
    }

    #[test]
    fn miri_partition_names_one_part_of_the_suite_wherever_it_stands() {
        let expected = Command::Run {
            names: vec!["miri"],
            json: false,
            fast: false,
            ci: true,
            no_cache: false,
            miri_part: Some(Part { index: 2, of: 12 }),
        };
        assert_eq!(
            parse(&["run", "--miri-partition=2/12", "--ci", "miri"]),
            expected
        );
        assert_eq!(
            parse(&["run", "--ci", "miri", "--miri-partition=2/12"]),
            expected
        );
        for (asked, index, of) in [("1/1", 1, 1), ("3/3", 3, 3)] {
            let flag = format!("--miri-partition={asked}");
            let whole = Command::Run {
                names: vec![],
                json: false,
                fast: false,
                ci: false,
                no_cache: false,
                miri_part: Some(Part { index, of }),
            };
            assert_eq!(parse(&["run", &flag]), whole, "{flag}");
        }
    }

    #[test]
    fn a_miri_partition_outside_one_to_n_or_given_twice_is_refused() {
        for wrong in ["0/3", "4/3", "1/0", "3", "a/3", "2/b", ""] {
            let flag = format!("--miri-partition={wrong}");
            let said = format!("`--miri-partition` takes K/N with K from 1 to N, not `{wrong}`");
            assert_eq!(parse(&["run", &flag]), Command::Usage(said));
        }
        let bare = Command::Usage("`--miri-partition` takes K/N with K from 1 to N, not ``".into());
        assert_eq!(parse(&["run", "--miri-partition", "2/3"]), bare);
        let twice = Command::Usage("`--miri-partition` is given twice".to_string());
        let flags = ["run", "--miri-partition=1/2", "--miri-partition=2/2"];
        assert_eq!(parse(&flags), twice);
    }

    #[test]
    fn cache_takes_the_one_word_clear() {
        assert_eq!(parse(&["cache", "clear"]), Command::CacheClear);
        let usage = Command::Usage("cache takes one word: clear".to_string());
        assert_eq!(parse(&["cache"]), usage);
        assert_eq!(parse(&["cache", "purge"]), usage);
        assert_eq!(parse(&["cache", "clear", "now"]), usage);
    }

    #[test]
    fn json_is_accepted_by_every_command_that_reports() {
        assert_eq!(parse(&["doctor", "--json"]), Command::Doctor { json: true });
        assert_eq!(parse(&["gates", "--json"]), Command::Gates { json: true });
        assert_eq!(parse(&["slop", "--json"]), Command::Slop(&["--json"]));
    }

    #[test]
    fn an_unknown_option_to_run_is_named_rather_than_ignored() {
        assert_eq!(
            parse(&["run", "--deep"]),
            Command::Usage("unknown option `--deep`".to_string())
        );
        assert_eq!(
            parse(&["run", "--all"]),
            Command::Usage("unknown option `--all`".to_string())
        );
    }

    #[test]
    fn a_command_that_takes_no_argument_says_so_rather_than_ignoring_one() {
        assert_eq!(
            parse(&["gates", "all"]),
            Command::Usage("gates takes no argument `all`".to_string())
        );
        assert_eq!(
            parse(&["doctor", "--global"]),
            Command::Usage("doctor takes no argument `--global`".to_string())
        );
    }

    #[test]
    fn no_arguments_and_an_unknown_command_each_say_which_it_was() {
        assert_eq!(parse(&[]), Command::Usage("no command given".to_string()));
        assert_eq!(
            parse(&["doctr"]),
            Command::Usage("unknown command `doctr`".to_string())
        );
    }

    #[test]
    fn baseline_can_be_narrowed_to_named_gates() {
        assert_eq!(
            parse(&["baseline", "slop"]),
            Command::Baseline(vec!["slop"])
        );
    }

    #[test]
    fn enable_and_disable_take_the_gates_they_are_given() {
        assert_eq!(
            parse(&["enable", "crap", "typos"]),
            Command::Configure {
                names: vec!["crap", "typos"],
                change: Change::On
            }
        );
        assert_eq!(
            parse(&["disable", "crap"]),
            Command::Configure {
                names: vec!["crap"],
                change: Change::Off(None)
            }
        );
        assert_eq!(
            parse(&["disable", "crap", "--reason", "too slow here", "typos"]),
            Command::Configure {
                names: vec!["crap", "typos"],
                change: Change::Off(Some("too slow here"))
            }
        );
        assert_eq!(
            parse(&["enable"]),
            Command::Usage("enable needs at least one gate name".to_string())
        );
    }

    #[test]
    fn stage_takes_gates_and_then_the_stage() {
        assert_eq!(
            parse(&["stage", "binsize", "bsize", "ci"]),
            Command::Configure {
                names: vec!["binsize", "bsize"],
                change: Change::Stage(Stage::Ci)
            }
        );
        assert_eq!(
            parse(&["stage", "binsize", "later"]),
            Command::Usage(
                "`later` is not a stage; the stages are commit, push, ci and manual".into()
            )
        );
        let short =
            Command::Usage("stage needs gate names, then commit, push, ci or manual".into());
        assert_eq!(parse(&["stage", "ci"]), short);
        assert_eq!(parse(&["stage"]), short);
    }

    /// The reason is the record of a hand decision, so an empty or a missing one is refused.
    #[test]
    fn a_reason_needs_its_text_and_other_options_are_refused() {
        let missing = Command::Usage("--reason needs the reason after it".to_string());
        assert_eq!(parse(&["disable", "crap", "--reason"]), missing);
        assert_eq!(parse(&["disable", "crap", "--reason", " "]), missing);
        assert_eq!(
            parse(&["disable", "crap", "--why"]),
            Command::Usage("unknown option `--why`".to_string())
        );
        assert_eq!(
            parse(&["disable", "--reason", "slow"]),
            Command::Usage("disable needs at least one gate name".to_string())
        );
    }

    /// `--help` after any command in the usage prints usage; `init` answers it with its own text.
    #[test]
    fn help_after_any_command_prints_usage() {
        let commands = USAGE
            .lines()
            .filter_map(|line| line.strip_prefix("  chock ")?.split_whitespace().next())
            .filter(|command| *command != "init");
        for command in commands {
            for help in ["--help", "-h"] {
                let usage = Command::Print(USAGE);
                assert_eq!(parse(&[command, help]), usage, "{command} {help}");
                assert_eq!(parse(&[command, "x", help]), usage, "{command} x {help}");
            }
        }
    }

    /// An empty switch would report success and change nothing.
    #[test]
    fn switching_with_no_gate_named_is_refused() {
        assert_eq!(
            parse(&["enable"]),
            Command::Usage("enable needs at least one gate name".to_string())
        );
        assert_eq!(
            parse(&["disable"]),
            Command::Usage("disable needs at least one gate name".to_string())
        );
    }
}
