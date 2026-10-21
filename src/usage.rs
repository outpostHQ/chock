//! The text `chock --help` prints, and every refused command beneath its reason.

/// What `chock --version` prints.
pub(crate) const VERSION_LINE: &str = concat!("chock ", env!("CARGO_PKG_VERSION"), "\n");

pub(crate) const USAGE: &str = "\
▰ chock — quality gates for Rust
  New code must be clean, and old debt has a limit that each fix lowers.

Set up
  chock init                      set up this project, then install the tools it pins
  chock init --global             only the tools, once per machine
  chock init --local              only this project: pins, config and hooks
  chock baseline [GATE...]        accept today's numbers as the record, also where they got worse
  chock doctor                    does this machine have the tools this project pins?

Every day
  chock run [GATE...]             run the named gates; no name means every gate that is on
  chock run --fast                only the gates a commit runs: quick, and none of them compiles
  chock run --ci                  the gates a CI job runs: staged for commit, push or ci
  chock run --no-cache            judge every gate again; no kept verdict answers
  chock run --verbose             a line for every gate, also for each one that passed
  chock run --miri-partition=K/N  run only part K of N of the Miri suite
  chock run --skip=GATE,...       run every gate but the ones named; CI runs those in other jobs
  chock run test --package=NAME   run the suite of one package only
  chock run test --filter=TEXT    run only the tests whose name holds TEXT
  chock run mutest --mutate=FILE,...  mutate only the named files
  chock run tla --model=NAME,...  check and prove only the named TLA+ models
  chock cache clear               remove every kept verdict, so the next run judges every gate
  chock explain GATE              the findings of the last run, without a new run
  chock explain                   all the debt in the record, largest first, with the fix for each

Choose the gates
  chock gates                     every gate: what it measures, and whether it is on
  chock enable GATE...            switch gates on
  chock disable GATE...           switch gates off; `--reason WHY` goes into the config
  chock stage GATE... STAGE       when they run: commit, push, ci or manual (only `chock run`)
  chock survey                    what every gate measures here; it changes no config and no record
  chock baseline --lower GATE...  record a gain on a gate that measures differently by machine
  chock baseline GATE --keys KEY  write only these keys, such as a moved file's; none goes up
  chock baseline --moved OLD=NEW  give a renamed file the keys of its old path; none goes up

Called by the hooks
  chock message FILE              check one commit message
  chock edited PATH...            check each file's own text, as the editor hook does
  chock slop [DIR]                list the comment blocks longer than the limit

Cut code, then show that nothing changed
  chock lean [DIR]                every line this tree could lose, file by file; no record
  chock lean --tests --min N      count test code too; list only files with N lines to lose
  chock oracle --old A --new B --corpus FILE
                                  run two builds over the same scenarios and compare each answer
  chock sweep REV [PATH...]       prove that only comments changed since REV, file by file
  chock moved REV PATH...         prove that code only moved between the files since REV

Options
  --json                          machine-readable output, for an agent or a CI job
  -h, --help                      this text; `chock COMMAND --help` shows one command
  -V, --version                   the version of chock

Exit codes: 0 every gate passed, 1 a gate tripped, 2 a gate could not run.
";

/// The line that follows a part of the usage, to say where the rest is.
pub(crate) const EVERY: &str = "`chock --help` shows every command.\n";

/// The commands the usage shows, in its order.
pub(crate) fn commands() -> Vec<&'static str> {
    let mut commands: Vec<&str> = Vec::new();
    for command in USAGE
        .lines()
        .filter_map(|line| line.strip_prefix("  chock ")?.split_whitespace().next())
    {
        if !commands.contains(&command) {
            commands.push(command);
        }
    }
    commands
}

/// The lines of the usage that show `command`, then where the rest is; `None` for no command.
pub(crate) fn about(command: &str) -> Option<String> {
    let lines = shown_in(USAGE, command);
    (!lines.is_empty()).then(|| format!("{}\n\n{EVERY}", lines.join("\n")))
}

/// The lines of `usage` that show `command`, each with the lines that go on from it.
fn shown_in<'u>(usage: &'u str, command: &str) -> Vec<&'u str> {
    let shown = format!("  chock {command}");
    let mut taking = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in usage.lines() {
        taking = match line.strip_prefix(&shown) {
            Some(rest) => rest.is_empty() || rest.starts_with(' '),
            // A long command's text goes on in a line of its own, indented deeper.
            None => taking && line.starts_with("    "),
        };
        if taking {
            lines.push(line);
        }
    }
    lines
}

/// What follows a problem: the lines of the command it came from, or all of the usage when it came
/// from none.
pub(crate) fn after_problem(command: Option<&str>) -> String {
    match command {
        None => USAGE.to_string(),
        Some(command) => about(command).unwrap_or_else(|| EVERY.to_string()),
    }
}

/// "; did you mean `{shown}WORD`?" for the word of `known` that `typed` most likely meant; nothing
/// where no word is near.
pub(crate) fn guess(typed: &str, known: &[&str], shown: &str) -> String {
    match closest(typed, known) {
        Some(meant) => format!("; did you mean `{shown}{meant}`?"),
        None => String::new(),
    }
}

/// The word of `known` that `typed` most likely meant: about one edit in every three letters.
pub(crate) fn closest<'k>(typed: &str, known: &[&'k str]) -> Option<&'k str> {
    let most = (typed.chars().count() + 1) / 3;
    known
        .iter()
        .map(|word| (edits(typed, word), *word))
        .filter(|(edits, _)| *edits <= most.max(1))
        .min_by_key(|(edits, _)| *edits)
        .map(|(_, word)| word)
}

/// How many letters to add, remove or change to make `a` into `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, letter) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, other) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(letter != *other));
            diagonal = above;
        }
    }
    row[b.len()]
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_command_s_help_is_its_own_lines_and_the_way_to_the_rest() {
        let run = about("run").unwrap();
        assert!(run.starts_with("  chock run [GATE...]"), "{run}");
        assert!(run.contains("chock run --skip=GATE,..."));
        assert!(!run.contains("chock cache clear"));
        assert!(run.ends_with(EVERY));
        let oracle = about("oracle").unwrap();
        assert!(
            oracle.contains("run two builds over the same scenarios"),
            "{oracle}"
        );
        assert_eq!(about("runner"), None);
        assert_eq!(about("nothing"), None);
        let usage = "    before\n  chock x\n    more\n  chock xy z\n  chock x  y\n";
        assert_eq!(
            shown_in(usage, "x"),
            ["  chock x", "    more", "  chock x  y"]
        );
        assert_eq!(after_problem(Some("run")), run);
        assert_eq!(after_problem(Some("nothing")), EVERY);
        assert_eq!(after_problem(None), USAGE);
    }

    #[test]
    fn a_typo_finds_the_word_it_missed_and_a_stranger_finds_none() {
        let known = commands();
        assert_eq!(closest("doctr", &known), Some("doctor"));
        assert_eq!(closest("baseine", &known), Some("baseline"));
        assert_eq!(closest("deploy", &known), None);
        assert_eq!(closest("lnks", &["lint", "links", "lines"]), Some("links"));
        let guessed = guess("doctr", &known, "chock ");
        assert_eq!(guessed, "; did you mean `chock doctor`?");
        assert_eq!(guess("deploy", &known, "chock "), "");
        assert_eq!(edits("kitten", "sitting"), 3);
        assert_eq!((edits("ab", "b"), edits("abc", "")), (1, 3));
        // Five letters may hold two edits.
        assert_eq!(closest("gtaes", &["gates"]), Some("gates"));
    }
}
