//! The echo gate: a comment block whose every word stands in the code line under it, counted per
//! file. docs/gates.md holds the rule whole.

use std::collections::BTreeSet;
use std::path::Path;

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Gate, Group, Kind, Measurement};
use crate::slop;

pub const GATE: Gate = Gate {
    name: "echoes",
    about: "comments that say only what the code line under them says, per file",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "comment(s) that echo their line",
    },
};

/// The words a comment says that no line needs to: articles, joins, and the verbs of a getter.
const FILLER: [&str; 36] = [
    "a", "an", "and", "are", "as", "at", "be", "been", "by", "create", "for", "from", "get",
    "given", "if", "in", "is", "it", "its", "new", "of", "on", "or", "return", "set", "that",
    "the", "these", "this", "those", "to", "value", "when", "whether", "which", "with",
];

/// The tags that open a note for a reader even with no colon after them.
const TAGS: [&str; 4] = ["TODO", "FIXME", "XXX", "HACK"];

const FIX: &str = "say what the line under the comment does not say, or remove the comment";

/// Each file's echoes: their count, and the place of each with the line it echoes.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let entered = |name: &str| !slop::skipped(name);
    let commented = |_: &str, path: &Path| slop::markers_for(&path.to_string_lossy()).is_some();
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &commented)?;
    let mut read = Measurement::of(Series::new(), Vec::new());
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        if let Some(detail) = in_file(&path, &shown)? {
            read.series.set(&shown, detail.places.len() as u64);
            read.details.insert(shown, detail);
        }
    }
    Ok(read)
}

/// One file's echoes, each placed through the line it echoes; none in a file that has none.
fn in_file(path: &Path, shown: &str) -> Result<Option<Detail>, String> {
    let Some((src, marks)) = slop::commented(path)? else {
        return Ok(None);
    };
    let quotes: &[char] = if shown.ends_with(".rs") {
        &['"']
    } else {
        &['"', '\'', '`']
    };
    let echo = |(first, code)| Place::at("echo", shown, first).through(code);
    let places: Vec<Place> = echoes(&src, marks, quotes).into_iter().map(echo).collect();
    let line = places.first().map(|place| place.line);
    Ok(line.map(|line| Detail {
        line: Some(line),
        places,
        fix: Some(FIX.to_string()),
    }))
}

/// Each block of `src` that echoes the code line under it: the numbers of its first line and of
/// that code line. `quotes` open the strings of the language, whose words a line does not say.
fn echoes(src: &str, marks: &[&str], quotes: &[char]) -> Vec<(u32, u32)> {
    let lines: Vec<&str> = src.lines().map(str::trim).collect();
    let (mut found, mut at) = (Vec::new(), 0);
    while let Some(start) = (at..lines.len()).find(|&i| opener(lines[i], marks).is_some()) {
        let mark = opener(lines[start], marks).unwrap_or_default();
        let end = (start..lines.len()).find(|&i| opener(lines[i], marks) != Some(mark));
        let end = end.unwrap_or(lines.len());
        let said: Vec<&str> = lines[start..end]
            .iter()
            .map(|line| &line[mark.len()..])
            .collect();
        let code = under(&lines, end, marks).filter(|&code| echoed(&said, lines[code], quotes));
        found.extend(code.map(|code| (number(start), number(code))));
        at = end;
    }
    found
}

fn number(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// The marker a comment line opens with; an inner doc, a directive (`//@`) or a shebang opens none.
fn opener<'m>(line: &str, marks: &[&'m str]) -> Option<&'m str> {
    let other = ["//!", "//@", "#!"].iter().any(|not| line.starts_with(not));
    let mark = marks.iter().find(|mark| line.starts_with(**mark));
    mark.copied().filter(|_| !other)
}

/// The index of the code line at `from` or below it past attributes and decorators; none where a
/// blank line or a comment comes first.
fn under(lines: &[&str], from: usize, marks: &[&str]) -> Option<usize> {
    let mut open = 0;
    let (at, line) = lines.iter().enumerate().skip(from).find(|(_, line)| {
        let attribute = open > 0 || line.starts_with("#[") || line.starts_with('@');
        let count = |of: &[char]| line.chars().filter(|c| of.contains(c)).count();
        open = (open + count(&['[', '('])).saturating_sub(count(&[']', ')']));
        !attribute
    })?;
    let comment = marks.iter().any(|mark| line.starts_with(mark));
    (!line.is_empty() && !comment).then_some(at)
}

/// Whether every word `said` stands in `code`, where `said` is no note, heading or code fence.
fn echoed(said: &[&str], code: &str, quotes: &[char]) -> bool {
    let text = said.join(" ");
    let shaped = |line: &&str| line.trim_start().starts_with('#') || line.contains("```");
    let told = words(&text);
    !tagged(text.trim())
        && !said.iter().any(shaped)
        && !told.is_empty()
        && told.is_subset(&words(&unquoted(code, quotes)))
}

/// Whether a block opens with a note's tag, such as `SAFETY:` or `TODO`.
fn tagged(text: &str) -> bool {
    let tag: String = text.chars().take_while(char::is_ascii_uppercase).collect();
    let rest = &text[tag.len()..];
    tag.len() > 1 && (rest.starts_with([':', '(']) || TAGS.contains(&tag.as_str()))
}

/// `code` with each string blanked, so a word in a string is no word the line says.
fn unquoted(code: &str, quotes: &[char]) -> String {
    let (mut out, mut open, mut escaped) = (String::new(), None, false);
    for c in code.chars() {
        match open {
            None if quotes.contains(&c) => open = Some(c),
            None => out.push(c),
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(quote) if c == quote => {
                open = None;
                out.push(' ');
            }
            Some(_) => {}
        }
    }
    out
}

/// The words of `text`, singular and in lower case, with no filler word.
fn words(text: &str) -> BTreeSet<String> {
    let runs = text.split(|c: char| !c.is_alphanumeric());
    let all = runs.flat_map(parts).map(|word| singular(&word));
    all.filter(|word| !word.is_empty() && !FILLER.contains(&word.as_str()))
        .collect()
}

/// A run split where its case turns, in lower case: `HTTPServer2` is `http` and `server2`.
fn parts(run: &str) -> Vec<String> {
    let chars: Vec<char> = run.chars().collect();
    let mut parts = vec![String::new()];
    for (at, &c) in chars.iter().enumerate() {
        let (before, next) = (at.checked_sub(1).map(|at| chars[at]), chars.get(at + 1));
        let acronym_ends = |b: char| b.is_uppercase() && next.is_some_and(|n| n.is_lowercase());
        if c.is_uppercase() && before.is_some_and(|b| !b.is_uppercase() || acronym_ends(b)) {
            parts.push(String::new());
        }
        if let Some(part) = parts.last_mut() {
            part.extend(c.to_lowercase());
        }
    }
    parts
}

/// `word` in the singular where `ies` or a plain `s` ends it: `entries` is `entry`; `status` stays.
fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies").filter(|stem| stem.len() > 1) {
        return format!("{stem}y");
    }
    match word.strip_suffix('s') {
        Some(stem) if stem.len() > 2 && !stem.ends_with(['s', 'u', 'i']) => stem.to_string(),
        _ => word.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const RUST: &[&str] = &["///", "//!", "//"];
    const HASH: &[&str] = &["#"];

    fn rust(src: &str) -> Vec<(u32, u32)> {
        echoes(src, RUST, &['"'])
    }

    fn hashed(src: &str) -> Vec<(u32, u32)> {
        echoes(src, HASH, &['"', '\'', '`'])
    }

    #[test]
    fn a_doc_that_only_names_its_function_echoes_it() {
        assert_eq!(
            rust("/// The config path.\nfn config_path() {}\n"),
            [(1, 2)]
        );
    }

    #[test]
    fn a_word_the_line_does_not_say_makes_the_comment_no_echo() {
        assert_eq!(
            rust("/// The config path, made absolute.\nfn config_path() {}\n"),
            []
        );
    }

    #[test]
    fn a_block_echoes_the_line_under_its_attributes() {
        let src = "/// The\n/// config path.\n#[must_use]\n#[allow(\n    dead_code,\n)]\nfn config_path() {}\n";
        assert_eq!(rust(src), [(1, 7)]);
    }

    #[test]
    fn a_blank_line_another_comment_or_the_end_under_a_block_leaves_it_no_line_to_echo() {
        assert_eq!(rust("/// The name.\n\nconst NAME: u8 = 1;\n"), []);
        assert_eq!(
            rust("/// The name.\n// The name, made short.\nconst NAME: u8 = 1;\n"),
            []
        );
        assert_eq!(rust("const NAME: u8 = 1;\n/// The name.\n"), []);
    }

    #[test]
    fn a_word_in_a_string_is_no_word_the_line_says() {
        assert_eq!(
            rust("/// The help text.\nconst HELP: &str = \"the help text\";\n"),
            []
        );
        assert_eq!(unquoted(r#"f("a \" b", c)"#, &['"']), "f( , c)");
        assert_eq!(unquoted(r#"f("", c)"#, &['"']), "f( , c)");
    }

    #[test]
    fn an_inner_doc_a_note_a_heading_or_a_fence_is_never_an_echo() {
        assert_eq!(rust("//! The config path.\nfn config_path() {}\n"), []);
        assert_eq!(rust("// SAFETY: the pointer.\nunsafe { pointer() }\n"), []);
        assert_eq!(rust("// TODO the config path\nfn config_path() {}\n"), []);
        assert_eq!(rust("/// # Errors\nfn errors() {}\n"), []);
        assert_eq!(
            rust("/// ```\n/// config_path()\n/// ```\nfn config_path() {}\n"),
            []
        );
    }

    #[test]
    fn words_match_across_case_plural_and_filler() {
        let src = "/// Returns the entries of the HTTPServer.\nfn http_server_entry() {}\n";
        assert_eq!(rust(src), [(1, 2)]);
        assert_eq!(parts("HTTPServer2"), ["http", "server2"]);
        assert_eq!(singular("status"), "status");
        assert_eq!(singular("gas"), "gas");
    }

    #[test]
    fn a_hash_comment_echoes_past_a_decorator_and_a_shebang_is_no_comment() {
        assert_eq!(hashed("# The name\nname = 'chock'\n"), [(1, 2)]);
        assert_eq!(
            hashed("# The handler.\n@route(\"/\")\ndef handler():\n"),
            [(1, 3)]
        );
        assert_eq!(hashed("#!/bin/sh\nsh\n"), []);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_file_is_keyed_by_its_path_with_a_place_through_each_echoed_line() {
        let ctx = crate::testdir::Held::tree(
            "gate-echoes",
            &[
                (
                    "src/a.rs",
                    "/// The config path.\nfn config_path() {}\n// Name.\nconst NAME: u8 = 1;\n",
                ),
                ("src/b.rs", "/// Reads the config.\nfn config_path() {}\n"),
                (
                    "src/c.rs",
                    "// slop-ok-file: made\n/// The name.\nconst NAME: u8 = 1;\n",
                ),
                ("src/d.py", "# Load the config.\nload('config')\n"),
            ],
        );
        let read = measure(&ctx).unwrap();
        assert_eq!(read.series, Series([("src/a.rs".to_string(), 2)].into()));
        let detail = &read.details["src/a.rs"];
        assert_eq!(detail.line, Some(1));
        let spans: Vec<_> = detail
            .places
            .iter()
            .map(|place| (place.line, place.last))
            .collect();
        assert_eq!(spans, [(1, Some(2)), (3, Some(4))]);
    }
}
