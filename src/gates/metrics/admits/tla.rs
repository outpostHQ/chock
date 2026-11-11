//! TLA+ text as the gates read it: comments blanked in place, and the units that start at
//! column 0, such as one definition, one `ASSUME` or one theorem with its proof.

/// TLA+ text with each character of its `\*` and `(* *)` comments blanked to a space and each line
/// kept, so a line number and a column 0 in the text are the same in the result.
#[must_use]
pub(crate) fn uncommented(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = [rest.find("\\*"), rest.find("(*")]
        .into_iter()
        .flatten()
        .min()
    {
        let (kept, comment) = rest.split_at(at);
        out.push_str(kept);
        let (close, skip) = if comment.starts_with("\\*") {
            ("\n", 0)
        } else {
            ("*)", 2)
        };
        let body = comment.get(2..).unwrap_or_default();
        let end = body.find(close).map_or(comment.len(), |end| 2 + end + skip);
        let (blanked, after) = comment.split_at(end);
        out.extend(blanked.chars().map(|c| if c == '\n' { c } else { ' ' }));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A line that starts at column 0, with the lines under it that start with a space or are blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// The 1-based line it starts on.
    pub line: usize,
    pub text: String,
}

impl Unit {
    /// Its first word, such as `ASSUME` or the name a definition defines.
    #[must_use]
    pub(crate) fn first(&self) -> &str {
        words(&self.text).next().unwrap_or_default()
    }
}

/// The units of uncommented TLA+ text, in order. Lines before the first unit belong to none.
#[must_use]
pub(crate) fn units(text: &str) -> Vec<Unit> {
    let mut found: Vec<Unit> = Vec::new();
    for (at, line) in text.lines().enumerate() {
        match found.last_mut() {
            Some(unit) if !line.starts_with(|c: char| !c.is_whitespace()) => {
                unit.text.push('\n');
                unit.text.push_str(line);
            }
            _ if line.starts_with(|c: char| !c.is_whitespace()) => found.push(Unit {
                line: at + 1,
                text: line.to_string(),
            }),
            _ => {}
        }
    }
    found
}

/// The names in TLA+ text: each run of letters, digits and `_`.
pub(crate) fn words(text: &str) -> impl Iterator<Item = &str> {
    let split = text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    split.filter(|word| !word.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_blanked_in_place_whichever_kind_comes_first() {
        assert_eq!(
            uncommented("a \\* (* b\nc"),
            format!("a {}\nc", " ".repeat(7))
        );
        assert_eq!(
            uncommented("a (* \\* b *) c"),
            format!("a {} c", " ".repeat(10))
        );
        assert_eq!(uncommented("a (* open"), format!("a {}", " ".repeat(7)));
        assert_eq!(uncommented("(* x\ny *)\nz \\* w"), "    \n    \nz     ");
    }

    #[test]
    fn a_unit_holds_its_column_0_line_and_the_indented_and_blank_lines_under_it() {
        let text = "  before\nA == 1\n  /\\ 2\n\nB == 3\n";
        let found = units(text);
        let unit = |line: usize, text: &str| Unit {
            line,
            text: text.to_string(),
        };
        assert_eq!(found, [unit(2, "A == 1\n  /\\ 2\n"), unit(5, "B == 3")]);
        assert_eq!(found[0].first(), "A");
        assert_eq!(unit(1, "---- MODULE M ----").first(), "MODULE");
        assert_eq!(unit(1, "").first(), "");
        assert_eq!(words("x_1 \\in S'").collect::<Vec<_>>(), ["x_1", "in", "S"]);
    }
}
