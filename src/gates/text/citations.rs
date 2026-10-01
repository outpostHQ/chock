//! Doc comments citing a file the repository does not have.

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "citations",
    about: "a doc comment naming a file this repository does not have, per file",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "stale citation(s)",
    },
};

/// The extensions a cited path must end in; any other token is prose.
const EXTENSIONS: [&str; 9] = [
    ".rs", ".toml", ".json", ".md", ".env", ".yml", ".yaml", ".lock", ".tsv",
];

/// Stale citations per file. A ratchet, since a file written at run time looks like one that moved.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut series = Series::new();
    for path in project::walked(
        &ctx.listing,
        &ctx.root,
        &|name| !project::SKIPPED.contains(&name),
        &|name, _| name.ends_with(".rs"),
    )? {
        let shown = project::relative(&ctx.root, &path);
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let stale = absent(&src, &|cited| anywhere(&ctx.root, cited));
        if stale > 0 {
            series.set(&shown, stale);
        }
    }
    Ok(series)
}

/// Whether any file's path ends in the citation, since a comment may write it relative to the
/// crate, `src/` or the workspace.
pub(crate) fn anywhere(root: &std::path::Path, cited: &str) -> bool {
    if root.join(cited).exists() {
        return true;
    }
    let ends = format!("/{cited}");
    project::walk(
        root,
        &|name| !project::SKIPPED.contains(&name),
        &|_, path| path.to_string_lossy().ends_with(&ends),
    )
    .is_ok_and(|found| !found.is_empty())
}

/// Version-control directories; a path under one is per-clone state no repository tracks.
const CONTROL: [&str; 3] = [".outpost/", ".git/", ".jj/"];

/// Whether this citation names run-time state rather than a file the repository holds.
#[must_use]
fn run_time_state(cited: &str) -> bool {
    CONTROL.iter().any(|held| cited.starts_with(held))
}

/// A path under a dot-directory this repository lacks, such as `.cargo/`, describes the reader's
/// own tree.
#[must_use]
fn convention(cited: &str, held: &dyn Fn(&str) -> bool) -> bool {
    let first = cited.split('/').next().unwrap_or(cited);
    first.starts_with('.') && !held(first)
}

/// How many paths this documentation names that the repository does not have.
#[must_use]
pub fn absent(src: &str, held: &dyn Fn(&str) -> bool) -> u64 {
    missing(src, held).len().try_into().unwrap_or(u64::MAX)
}

/// Each path the `///` and `//!` comments cite that the repository lacks, with its line. Plain `//`
/// notes often name fixtures or test inputs, so they are not read.
#[must_use]
pub fn missing(src: &str, held: &dyn Fn(&str) -> bool) -> Vec<(usize, String)> {
    src.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| line.starts_with("///") || line.starts_with("//!"))
        .flat_map(|(at, line)| cited(line).into_iter().map(move |path| (at, path)))
        .filter(|(_, cited)| !run_time_state(cited))
        .filter(|(_, cited)| !convention(cited, held))
        .filter(|(_, cited)| !held(cited))
        .filter(|(_, cited)| !spelled_in_code(src, cited))
        .collect()
}

/// Whether the file's own code quotes this path, making it one the program reads or writes.
fn spelled_in_code(src: &str, cited: &str) -> bool {
    let quoted = format!("\"{cited}\"");
    src.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .any(|line| line.contains(&quoted))
}

/// The backticked tokens on a line that hold a slash, end in a known extension and are not
/// placeholders.
fn cited(line: &str) -> Vec<String> {
    line.split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| token.contains('/'))
        .filter(|token| EXTENSIONS.iter().any(|end| token.ends_with(end)))
        .filter(|token| !token.contains(['<', '>', '…', '*', ' ', '$', '{', '?']))
        .filter(|token| !stands_in(token))
        .map(str::to_string)
        .collect()
}

/// Whether the first directory is one character, as in `x/mod.rs`: a stand-in for a name.
fn stands_in(token: &str) -> bool {
    token
        .split('/')
        .next()
        .is_some_and(|first| first.chars().count() < 2)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn stale(src: &str, tree: &[&str]) -> u64 {
        absent(src, &|cited| tree.contains(&cited))
    }

    #[test]
    fn a_comment_naming_a_file_the_repository_has_counts_nothing() {
        assert_eq!(
            stale("/// See `src/run.rs` for the ratchet.\n", &["src/run.rs"]),
            0
        );
    }

    #[test]
    fn a_path_the_code_itself_writes_is_run_time_state_and_a_quote_in_a_comment_is_not() {
        let installs = "//! Installs `.claude/settings.local.json`.\n\
                        fn install(p: &Path) { p.join(\".claude/settings.local.json\"); }\n";
        assert_eq!(stale(installs, &[".claude"]), 0);
        let only_said = "//! See `docs/gone.md`.\n/// Or \"docs/gone.md\".\nfn f() {}\n";
        assert_eq!(
            stale(only_said, &[]),
            1,
            "a quote in a comment excuses nothing"
        );
    }

    #[test]
    fn a_comment_naming_run_time_state_under_a_control_directory_counts_nothing() {
        for named in [
            ".outpost/hooks.toml",
            ".outpost/git-mirror.lock",
            ".git/COMMIT_EDITMSG",
            ".jj/repo",
        ] {
            let said = format!("/// `{named}` is written per clone.\n");
            assert_eq!(stale(&said, &["src/run.rs"]), 0, "{named}");
        }
        // A directory named like a control directory is ordinary source and still counts.
        assert_eq!(stale("/// `outpost/config.toml` holds it.\n", &[]), 1);
    }

    #[test]
    fn a_note_inside_a_body_is_not_documentation_and_is_not_judged() {
        assert_eq!(stale("// `blocked/keep.rs` should be ignored.\n", &[]), 0);
        assert_eq!(stale("/// `blocked/keep.rs` should be ignored.\n", &[]), 1);
        assert_eq!(stale("//! `blocked/keep.rs` should be ignored.\n", &[]), 1);
        // A string that starts like a comment is not a comment.
        assert_eq!(stale("let x = \"//`aa/b.rs`\";\n", &[]), 0);
    }

    #[test]
    fn a_path_under_a_dot_directory_this_repository_does_not_have_is_a_convention() {
        assert_eq!(
            stale("/// in your project's `.cargo/config.toml`\n", &[]),
            0
        );
        // A repository that has the directory makes a claim about its own tree.
        assert_eq!(
            stale("/// see `.cargo/config.toml`\n", &[".cargo"]),
            1,
            "held directory, absent file"
        );
        assert_eq!(
            stale(
                "/// see `.cargo/config.toml`\n",
                &[".cargo", ".cargo/config.toml"]
            ),
            0
        );
    }

    #[test]
    fn a_comment_naming_a_file_from_somebody_elses_project_is_counted() {
        assert_eq!(
            stale("/// `storage/s3.rs` gated a helper.\n", &["src/run.rs"]),
            1
        );
    }

    #[test]
    fn a_path_with_no_directory_in_it_is_an_example_rather_than_a_citation() {
        assert_eq!(stale("/// looked for `x.rs` or `x/mod.rs`\n", &[]), 0);
    }

    #[test]
    fn a_one_letter_directory_is_a_stand_in_for_a_name() {
        assert_eq!(stale("/// as `d/y.rs` names it\n", &[]), 0);
        assert_eq!(stale("/// under `ab/y.rs`\n", &[]), 1);
    }

    #[test]
    fn a_placeholder_is_not_a_citation() {
        for shape in [
            "`<name>/thing.rs`",
            "`src/gates/*.rs`",
            "`$CARGO_HOME/config.toml`",
            "`tests/{one,two}.rs`",
        ] {
            assert_eq!(stale(&format!("/// see {shape}\n"), &[]), 0, "{shape}");
        }
    }

    #[test]
    fn a_token_that_is_not_a_path_is_left_alone() {
        assert_eq!(
            stale(
                "/// `Keys::Items` and `--json` and `cargo fmt --all`\n",
                &[]
            ),
            0
        );
    }

    #[test]
    fn a_path_outside_a_comment_is_not_a_citation() {
        assert_eq!(stale("const F: &str = \"`docs/gone.md`\";\n", &[]), 0);
    }

    #[test]
    fn every_extension_this_gate_rules_on_is_counted() {
        for end in EXTENSIONS {
            assert_eq!(
                stale(&format!("/// see `dir/thing{end}`\n"), &[]),
                1,
                "{end}"
            );
        }
    }

    #[test]
    fn each_citation_on_one_line_is_counted_separately() {
        assert_eq!(
            stale("/// `aa/one.rs` and `bb/two.rs`\n", &["aa/one.rs"]),
            1
        );
        assert_eq!(stale("/// `aa/one.rs` and `bb/two.rs`\n", &[]), 2);
    }
}
