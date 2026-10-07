//! The text `chock --help` prints, and every refused command beneath its reason.

/// What `chock --version` prints.
pub(crate) const VERSION_LINE: &str = concat!("chock ", env!("CARGO_PKG_VERSION"), "\n");

pub(crate) const USAGE: &str = "\
▰ chock — quality gates for Rust
  New code must be clean, and old debt has a limit that each fix lowers.

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
  chock run --no-cache            judge every gate again; no kept verdict answers
  chock run --miri-partition=K/N  run only part K of N of the Miri suite
  chock run --skip=GATE,...       run every gate but the ones named; CI runs those in other jobs
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
