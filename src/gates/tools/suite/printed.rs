//! What a harness of its own prints under a `test <name> ... FAILED` line, such as mutest-rs's UI
//! tests, whose diff is there and in no panic. A bounded tail of the run can lose it.

use std::collections::BTreeMap;

use crate::exec;

/// The most lines kept under one `... FAILED` line.
const UNDER: usize = 40;

/// The failed tests whose lines a reason quotes; it counts the rest.
const QUOTED: usize = 10;

/// What the harness printed under the `... FAILED` line of `test`, if it printed anything there.
pub(super) fn under(out: &exec::Output, test: &str) -> Option<String> {
    under_each(out).remove(test).map(|lines| lines.join("\n"))
}

/// Each failed test with the lines under its `... FAILED` line, as the end of a reason; nothing
/// where no such line has any. A test that both streams name keeps its longer quote.
pub(in crate::gates) fn quoted(out: &exec::Output) -> String {
    let each = under_each(out);
    if each.is_empty() {
        return String::new();
    }
    let mut said = String::from("\nwhat each failed test printed under its FAILED line:");
    for (test, lines) in each.iter().take(QUOTED) {
        said.extend(["\ntest ", test, " ... FAILED\n", &lines.join("\n")]);
    }
    let more = each.len().saturating_sub(QUOTED);
    if more > 0 {
        said.extend([
            "\n(",
            &more.to_string(),
            " more failed tests printed lines there)",
        ]);
    }
    said
}

/// Each test with a `... FAILED` line that has lines under it, up to the next verdict or summary.
fn under_each(out: &exec::Output) -> BTreeMap<String, Vec<String>> {
    let mut each: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for text in [&out.stdout, &out.stderr] {
        let lines: Vec<String> = (text.lines())
            .map(|line| exec::strip_colour(line).trim_end().to_string())
            .collect();
        for (at, line) in lines.iter().enumerate() {
            let Some(test) = failed(line) else {
                continue;
            };
            let mut below: Vec<String> = (lines[at + 1..].iter())
                .take_while(|line| !ends(line))
                .take(UNDER)
                .cloned()
                .collect();
            while below.last().is_some_and(|line| line.trim().is_empty()) {
                below.pop();
            }
            let longer = each.get(&test).is_none_or(|held| held.len() < below.len());
            if !below.is_empty() && longer {
                each.insert(test, below);
            }
        }
    }
    each
}

/// The test a `test <name> ... FAILED` line names.
fn failed(line: &str) -> Option<String> {
    let named = line
        .trim()
        .strip_prefix("test ")?
        .strip_suffix(" ... FAILED")?;
    Some(named.trim().to_string())
}

/// A line that ends what a failed test printed: the next test's verdict, or the run's summary.
fn ends(line: &str) -> bool {
    let line = line.trim();
    let verdict = line.starts_with("test ") && line.contains(" ... ");
    verdict || line.starts_with("test result:") || line == "failures:"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mutest-rs's UI harness: the reason and both streams under the line, on stderr.
    const UI: &str = "test a ... ok\n\u{1b}[1;31mtest ui/b ... FAILED\u{1b}[0m\n    exited with \
                      code 1\nstdout:\n\nstderr:\n- left\n+ right\n\ntest c ... FAILED\ntest d ... \
                      ok\n\nfailures:\n    ui/b\n    c\ntest result: FAILED. 2 passed; 2 failed\n";

    #[test]
    fn the_lines_under_a_failed_line_end_at_the_next_verdict_or_the_summary() {
        let out = exec::Output::of(Some(1), "", UI);
        let shown = "    exited with code 1\nstdout:\n\nstderr:\n- left\n+ right";
        assert_eq!(under(&out, "ui/b").as_deref(), Some(shown));
        assert_eq!(
            under(&out, "c"),
            None,
            "a line with none under it quotes nothing"
        );
        let reason = quoted(&out);
        let head = "\nwhat each failed test printed under its FAILED line:\ntest ui/b ... FAILED\n";
        assert_eq!(reason, format!("{head}{shown}"));
        assert_eq!(
            quoted(&exec::Output::of(Some(1), "test c ... FAILED\n", "")),
            ""
        );
    }

    #[test]
    fn a_long_quote_keeps_its_first_forty_lines_and_many_failures_are_counted() {
        let numbered = |count: usize| (0..count).map(|n| format!("line {n}")).collect::<Vec<_>>();
        let printed = numbered(60).join("\n");
        let out = exec::Output::of(Some(1), &format!("test t ... FAILED\n{printed}\n"), "");
        assert_eq!(under(&out, "t"), Some(numbered(UNDER).join("\n")));
        let failed = |n: usize| format!("test t{n:02} ... FAILED");
        let many = (0..12)
            .map(|n| format!("{}\n  why\n", failed(n)))
            .collect::<Vec<_>>();
        let said = quoted(&exec::Output::of(Some(1), "", &many.concat()));
        let named: Vec<&str> = said
            .lines()
            .filter(|line| line.ends_with("FAILED"))
            .collect();
        assert_eq!(named, (0..QUOTED).map(failed).collect::<Vec<_>>());
        assert!(
            said.ends_with("\n(2 more failed tests printed lines there)"),
            "{said}"
        );
    }

    #[test]
    fn a_test_both_streams_name_keeps_its_longer_quote() {
        let out = exec::Output::of(
            Some(1),
            "test t ... FAILED\n  a\n",
            "test t ... FAILED\n  a\n  b\n",
        );
        assert_eq!(under(&out, "t").as_deref(), Some("  a\n  b"));
    }
}
