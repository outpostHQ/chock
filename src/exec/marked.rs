//! The line a compiler or linker marked as its error, and the place it gave for it.

/// The first line a compiler or linker marked as an error, preferring a rustc diagnostic with a
/// code over cargo's closing summary.
pub(crate) fn marked(text: &str) -> Option<&str> {
    let lines = || text.lines().map(str::trim_start);
    lines()
        .find(|line| line.starts_with("error["))
        .or_else(|| lines().find(|line| line.starts_with("error:") && !a_summary(line)))
        .or_else(|| lines().find_map(|line| named_its_own_error(line)))
        .or_else(|| lines().find(|line| line.starts_with("fatal:")))
}

/// `marked`, with the place rustc prints on the line under it, as in `error[E0308]: mismatched
/// types at src/a.rs:4:20`.
pub(crate) fn marked_at(text: &str) -> Option<String> {
    let error = marked(text)?;
    let mut lines = text.lines().map(str::trim_start);
    let next = lines.by_ref().find(|line| *line == error).and(lines.next());
    Some(match next.and_then(|line| line.strip_prefix("--> ")) {
        Some(place) => format!("{error} at {place}"),
        None => error.to_string(),
    })
}

/// A `tool: error: …` line, as a linker such as `wild` prints.
fn named_its_own_error(line: &str) -> Option<&str> {
    let (before, _) = line.split_once("error: ")?;
    let tool = before.strip_suffix(": ")?;
    let named = !tool.is_empty() && !tool.contains(char::is_whitespace);
    (named && !a_summary(line)).then_some(line)
}

/// cargo's closing `could not compile … previous error` line, which names the crate, not the line.
fn a_summary(line: &str) -> bool {
    line.contains("could not compile") || line.contains("previous error")
}

pub(super) fn last_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim_end)
        .rfind(|line| !line.is_empty())
}
