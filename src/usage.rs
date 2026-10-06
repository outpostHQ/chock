//! The text `chock --help` prints, and every refused command beneath its reason.

pub(crate) const USAGE: &str = "\
▰ chock — quality gates for Rust: new code must be clean, and existing debt cannot grow.

Set up
  chock init                      set up this project, then install the tools it pins
  chock init --global             only the tools, once per machine
  chock init --local              only this project: justfile, pins, config and hooks
  chock baseline [GATE...]        accept today's numbers as the record, also where they got worse
  chock doctor                    does this machine have the tools this project pins?

Every day
  chock run [GATE...]             run the named gates; no name means every gate that is on
  chock run --fast                only the gates staged for a commit; they need no compile
  chock run --ci                  the gates a CI job runs: staged for commit, push or ci
  chock explain GATE              the findings of the last run, without a new run
  chock explain                   all the debt in the record, largest first, with the fix for each

Choose the gates
  chock gates                     every gate: what it measures, and whether it is on
  chock enable GATE...            switch gates on
  chock disable GATE...           switch gates off; `--reason WHY` goes into the config
  chock stage GATE... STAGE       when they run: commit, push, ci or manual (only `chock run`)
  chock survey                    what every gate measures here; it changes no config and no record
  chock baseline --lower GATE...  record a gain on a gate that measures differently by machine

Called by the hooks
  chock message FILE              check one commit message
  chock edited PATH...            check each file's own text, as the editor hook does
  chock slop [DIR]                list the comment blocks longer than the limit

Options
  --json                          machine-readable output, for an agent or a CI job
  -h, --help                      this text; `chock init --help` shows the options of init
  -V, --version                   the version of chock

Exit codes: 0 every gate passed, 1 a gate tripped, 2 a gate could not run.
";
