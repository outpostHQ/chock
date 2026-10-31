//! Characters no reader sees that change what a file tells a compiler, a shell or an AI agent: tag
//! characters (hidden prompts), mixed variation selectors (`GlassWorm`, 2026-03), bidi controls.

use crate::run::report::Finding;

/// The rule a hidden character breaks, as the finding's item.
pub(super) const RULE: &str = "hidden_unicode";

/// The waving black flag: the one character a run of tag characters may follow, as in the flag of
/// Scotland.
const FLAG: char = '\u{1F3F4}';

/// The first hidden character of each line, as a finding at that line.
pub(super) fn found(path: &str, text: &str) -> Vec<Finding> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (column, what) = first(line)?;
            let message = format!(
                "holds {what} at column {column}, which no reader sees: delete it, or write it \
                 as an escape"
            );
            let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
            Some(Finding::at(path, &message).line(line).item(RULE))
        })
        .collect()
}

/// The 1-based column, in characters, and the kind of the first hidden character in a line.
fn first(line: &str) -> Option<(usize, &'static str)> {
    let mut previous: Option<char> = None;
    // Whether the run of tag characters going on began right after `FLAG`.
    let mut flagged = false;
    for (index, c) in line.chars().enumerate() {
        let what = if is_tag(c) {
            flagged |= previous == Some(FLAG);
            (!flagged).then_some("a Unicode tag character")
        } else {
            flagged = false;
            // The same selector twice, as an emoji typed with its `U+FE0F` twice, carries no bytes.
            if is_selector(c) && previous.is_some_and(|before| is_selector(before) && before != c) {
                Some("two different variation selectors in a row")
            } else {
                is_bidi(c).then_some("a bidirectional control")
            }
        };
        if let Some(what) = what {
            return Some((index + 1, what));
        }
        previous = Some(c);
    }
    None
}

/// U+E0000 to U+E007F, which an emoji flag sequence uses after `FLAG` and nothing else should.
fn is_tag(c: char) -> bool {
    ('\u{E0000}'..='\u{E007F}').contains(&c)
}

/// One variation selector picks one glyph; text never needs two different ones in a row.
fn is_selector(c: char) -> bool {
    ('\u{FE00}'..='\u{FE0F}').contains(&c) || ('\u{E0100}'..='\u{E01EF}').contains(&c)
}

/// The embeddings, overrides and isolates of Trojan Source (CVE-2021-42574). The marks U+200E and
/// U+200F, which right-to-left text needs, are not among them.
fn is_bidi(c: char) -> bool {
    ('\u{202A}'..='\u{202E}').contains(&c) || ('\u{2066}'..='\u{2069}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each finding as its line and what it names, up to the column.
    fn kinds(text: &str) -> Vec<String> {
        found("README.md", text)
            .iter()
            .map(|finding| {
                let named = finding.message.split(", which").next().unwrap_or_default();
                format!("{}: {named}", finding.line.unwrap_or(0))
            })
            .collect()
    }

    #[test]
    fn a_tag_character_a_stacked_selector_and_a_bidi_control_are_each_named_at_their_line() {
        let text = "fine\nsee\u{E0041}\u{E0042} here\nx\u{FE0E}\u{FE0F}\nif a\u{202E} {\n/*\u{2067} } */\n";
        let named = "2: holds a Unicode tag character at column 4\n\
                     3: holds two different variation selectors in a row at column 3\n\
                     4: holds a bidirectional control at column 5\n\
                     5: holds a bidirectional control at column 3";
        assert_eq!(kinds(text).join("\n"), named);
    }

    #[test]
    fn emoji_flags_one_selector_or_the_same_twice_a_joiner_and_right_to_left_marks_pass() {
        let scotland = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
        let text = format!(
            "{scotland} heart \u{2764}\u{FE0F} tool \u{1F6E0}\u{FE0F}\u{FE0F} family \
             \u{1F468}\u{200D}\u{1F469} \u{5D0}\u{200F}\n"
        );
        assert_eq!(kinds(&text), Vec::<String>::new());
    }

    #[test]
    fn a_tag_run_after_any_other_character_is_hidden_text() {
        assert_eq!(
            kinds("\u{E0049}\u{E0047}\n"),
            ["1: holds a Unicode tag character at column 1"]
        );
        assert_eq!(
            kinds("a\u{1F3F4} b\u{E0067}\n"),
            ["1: holds a Unicode tag character at column 5"]
        );
    }
}
