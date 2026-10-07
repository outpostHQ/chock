//! Parsing `tool-versions.env`, and failing closed on anything it cannot read.
//! A half-read pin file makes `doctor` report green over tools it never looked at.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The key as written, e.g. `CARGO_NEXTEST_VERSION`.
    pub key: String,
    /// The crate `cargo install` knows it by, e.g. `cargo-nextest`.
    pub crate_name: String,
    /// The executable on `PATH`, which differs from the crate: `kani-verifier` installs `kani`, and
    /// `typos-cli` installs `typos`.
    pub command: String,
    /// The version this project pins.
    pub want: String,
    /// A command to run once the crate is installed, for a tool that fetches the rest of itself.
    /// `kani-verifier` installs a shim and pulls its solver and toolchain on first use.
    pub setup: Option<String>,
    /// The systems the tool runs on, as `std::env::consts::OS` names them; `None` is every one.
    /// cackle refuses to compile off Linux, and Kani publishes no Windows build.
    pub systems: Option<Vec<String>>,
}

/// The version a tool that is not on crates.io is pinned at. There is no coordinate to pin, so
/// `cargo install` cannot fetch it and `doctor` can only say whether the binary is there.
pub const UNPUBLISHED: &str = "0.0.0";

impl Pin {
    /// Whether this tool has to be built from a checkout rather than fetched.
    #[must_use]
    pub fn unpublished(&self) -> bool {
        self.want == UNPUBLISHED
    }

    /// Why `os` has no use for this tool, when its pin names the systems it runs on and `os` is
    /// not one of them.
    #[must_use]
    pub fn elsewhere(&self, os: &str) -> Option<String> {
        let systems = self.systems.as_ref()?;
        (!systems.iter().any(|system| system == os)).then(|| {
            format!(
                "runs only on {}, so {os} has no use for it",
                systems.join(", ")
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A line that is neither blank, a comment, nor `KEY=VALUE`.
    Unreadable {
        line: usize,
        text: String,
    },
    /// `KEY_VERSION=` with nothing after it.
    EmptyVersion {
        line: usize,
        key: String,
    },
    /// A value holding whitespace and no quotes. CI sources this file, where `K=a b` runs `b`
    /// with `K=a` set rather than assigning the string.
    Unquoted {
        line: usize,
        key: String,
    },
    /// `FOO_BIN=bar` or `FOO_SETUP=…` with no `FOO_VERSION` anywhere in the file.
    Orphan {
        line: usize,
        key: String,
        suffix: &'static str,
    },
    InvalidToolchain {
        line: usize,
        key: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { line, text } => {
                write!(f, "line {line}: expected KEY=VALUE, found `{text}`")
            }
            Self::EmptyVersion { line, key } | Self::InvalidToolchain { line, key } => {
                write!(f, "line {line}: {key} {}", self.invalid_value())
            }
            Self::Unquoted { line, key } => write!(
                f,
                "line {line}: {key} has spaces in its value and no quotes; a shell sourcing this \
                 file would run it instead of assigning it"
            ),
            Self::Orphan { line, key, suffix } => {
                write!(
                    f,
                    "line {line}: {key}{suffix} has no matching {key}_VERSION"
                )
            }
        }
    }
}

impl ParseError {
    fn invalid_value(&self) -> &'static str {
        if matches!(self, Self::EmptyVersion { .. }) {
            "pins no version"
        } else {
            "must name a nightly toolchain, not a command or path"
        }
    }
}

/// Whether a value is in matching quotes. CI sources this file, so a value the shell would take
/// as a command must be quoted.
fn quoted(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
}

fn unquote(value: &str) -> &str {
    if quoted(value) {
        return &value[1..value.len() - 1];
    }
    value
}

const VERSION_SUFFIX: &str = "_VERSION";
const BIN_SUFFIX: &str = "_BIN";
const SETUP_SUFFIX: &str = "_SETUP";
const OS_SUFFIX: &str = "_OS";
const NIGHTLY_SUFFIX: &str = "_NIGHTLY";
/// Keys that say something about a `*_VERSION` pin of the same stem, and mean nothing without one.
const QUALIFIERS: [&str; 3] = [BIN_SUFFIX, SETUP_SUFFIX, OS_SUFFIX];

/// Each qualifier's values by the stem they qualify, with the line each was set on.
type Qualified = BTreeMap<&'static str, BTreeMap<String, (usize, String)>>;

/// A qualifier key split into its suffix and the stem it names.
fn qualifier(key: &str) -> Option<(&'static str, &str)> {
    QUALIFIERS
        .iter()
        .find_map(|suffix| Some((*suffix, key.strip_suffix(suffix)?)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainPin {
    pub key: String,
    pub want: String,
}

/// Non-crate assignments must not become `cargo install` inputs or disappear from doctor.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdditionalPins {
    pub toolchains: Vec<ToolchainPin>,
    pub unchecked: Vec<String>,
}

pub fn additional(text: &str) -> Result<AdditionalPins, ParseError> {
    let mut found = AdditionalPins::default();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let Some((key, value)) = assignment(raw, line)? else {
            continue;
        };
        if key.ends_with(NIGHTLY_SUFFIX) {
            if !nightly_name(value) || key == NIGHTLY_SUFFIX {
                return Err(ParseError::InvalidToolchain {
                    line,
                    key: key.to_string(),
                });
            }
            found.toolchains.push(ToolchainPin {
                key: key.to_string(),
                want: value.to_string(),
            });
        } else if !key.ends_with(VERSION_SUFFIX) && qualifier(key).is_none() {
            found.unchecked.push(format!("line {line}: {key}"));
        }
    }
    Ok(found)
}

/// A pin file moved to the pins of the chock that runs: its new text, the chock the old text named,
/// and a line per change for `init` to report.
#[derive(Debug, PartialEq, Eq)]
pub struct Repinned {
    pub from: String,
    pub text: String,
    pub changes: Vec<String>,
}

/// `held` with each key `ours` sets taken from `ours`. A key only `held` sets is the project's own
/// and stays below `ours`, with its comments. `None` where `held` names no chock or will not parse.
#[must_use]
pub fn repinned(held: &str, ours: &str) -> Option<Repinned> {
    let (theirs, set) = (assignments(held)?, assignments(ours)?);
    let from = value_of(&theirs, "CHOCK_VERSION")?.to_string();
    let (own, kept) = own_lines(held, &set);
    let mut changes: Vec<String> = set
        .iter()
        .filter_map(|&(key, want)| match value_of(&theirs, key) {
            None => Some(format!("added {key}={want}")),
            Some(have) => (have != want).then(|| format!("{key} {have} -> {want}")),
        })
        .collect();
    if !kept.is_empty() {
        changes.push(format!("kept as this project's own: {}", kept.join(", ")));
    }
    Some(Repinned {
        from,
        text: format!("{ours}{own}"),
        changes,
    })
}

/// Every assignment in `text`, or `None` where a line is not one.
fn assignments(text: &str) -> Option<Vec<(&str, &str)>> {
    let mut found = Vec::new();
    for raw in text.lines() {
        found.extend(assignment(raw, 0).ok()?);
    }
    Some(found)
}

fn value_of<'a>(list: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    list.iter()
        .find(|(name, _)| *name == key)
        .map(|&(_, value)| value)
}

/// The lines of `held` that set a key `set` does not, each with the comments directly above it and
/// a blank line where one stood, and those keys in order.
fn own_lines<'a>(held: &'a str, set: &[(&str, &str)]) -> (String, Vec<&'a str>) {
    let (mut own, mut kept, mut block) = (String::new(), Vec::new(), Vec::new());
    // True at the start, so the first key the project keeps opens on a line of its own.
    let mut spaced = true;
    for raw in held.lines() {
        match assignment(raw, 0).ok().flatten() {
            None if raw.trim().is_empty() => {
                block.clear();
                spaced = true;
            }
            None => block.push(raw),
            Some((key, _)) if value_of(set, key).is_some() => block.clear(),
            Some((key, _)) => {
                block.push(raw);
                own.push_str(if spaced { "\n" } else { "" });
                own.push_str(&block.join("\n"));
                own.push('\n');
                (block, spaced) = (Vec::new(), false);
                kept.push(key);
            }
        }
    }
    (own, kept)
}

fn nightly_name(value: &str) -> bool {
    (value == "nightly" || value.starts_with("nightly-"))
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The first qualifier key whose stem no `*_VERSION` pins.
fn orphan(
    pinned: &[&String],
    suffix: &'static str,
    qualifiers: &BTreeMap<String, (usize, String)>,
) -> Option<ParseError> {
    qualifiers
        .iter()
        .find(|(stem, _)| !pinned.contains(stem))
        .map(|(stem, (line, _))| ParseError::Orphan {
            line: *line,
            key: stem.clone(),
            suffix,
        })
}

/// What a shell would read before the comment starts: a `#` that opens a word, and not one inside
/// quotes. `A=1 # why` assigns `1`, `A=a#b` assigns `a#b`, and `A="a # b"` is one value.
fn before_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(open) if b == open => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return line[..i].trim_end();
            }
            None => {}
        }
    }
    line
}

/// One `KEY=VALUE` line, or `None` for a blank or a comment. The shell quoting is settled here so
/// the caller sees the value a shell would have assigned.
fn assignment(raw: &str, line: usize) -> Result<Option<(&str, &str)>, ParseError> {
    let trimmed = before_comment(raw.trim());
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some((key, value)) = trimmed.split_once('=') else {
        return Err(ParseError::Unreadable {
            line,
            text: trimmed.to_string(),
        });
    };
    let (key, value) = (key.trim(), value.trim());
    if value.contains(char::is_whitespace) && !quoted(value) {
        return Err(ParseError::Unquoted {
            line,
            key: key.to_string(),
        });
    }
    Ok(Some((key, unquote(value))))
}

/// `*_VERSION` pins, `*_BIN` overrides the executable name, `*_SETUP` names a command to run after
/// installing. Data here rather than a table, so a new tool needs no chock release.
pub fn parse(text: &str) -> Result<Vec<Pin>, ParseError> {
    let mut versions: Vec<(usize, String, String)> = Vec::new();
    let mut qualified = Qualified::new();

    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let Some((key, value)) = assignment(raw, line)? else {
            continue;
        };
        if let Some(stem) = key.strip_suffix(VERSION_SUFFIX) {
            if value.is_empty() {
                return Err(ParseError::EmptyVersion {
                    line,
                    key: key.to_string(),
                });
            }
            versions.push((line, stem.to_string(), value.to_string()));
        } else if let Some((suffix, stem)) = qualifier(key) {
            qualified
                .entry(suffix)
                .or_default()
                .insert(stem.to_string(), (line, value.to_string()));
        }
    }

    let pinned: Vec<&String> = versions.iter().map(|(_, stem, _)| stem).collect();
    let unpinned = |suffix| orphan(&pinned, suffix, qualified.get(suffix)?);
    if let Some(orphan) = QUALIFIERS.into_iter().find_map(unpinned) {
        return Err(orphan);
    }
    let said = |suffix, stem: &String| {
        qualified
            .get(suffix)?
            .get(stem)
            .map(|(_, value)| value.clone())
    };

    Ok(versions
        .into_iter()
        .map(|(_, stem, want)| {
            let crate_name = stem.to_lowercase().replace('_', "-");
            let command = said(BIN_SUFFIX, &stem).unwrap_or_else(|| crate_name.clone());
            let systems = said(OS_SUFFIX, &stem).map(|list| systems(&list));
            Pin {
                key: format!("{stem}{VERSION_SUFFIX}"),
                crate_name,
                command,
                want,
                setup: said(SETUP_SUFFIX, &stem),
                systems,
            }
        })
        .collect())
}

/// `linux,macos`: a comma list, because a value with a space in it is a command to the shell that
/// sources this file.
fn systems(list: &str) -> Vec<String> {
    list.split(',')
        .map(|system| system.trim().to_string())
        .collect()
}

/// The command of each pin in the shipped file, in the file's order.
#[cfg(test)]
pub const SHIPPED: [&str; 15] = [
    "cargo-binstall",
    "just",
    "cargo-nextest",
    "cargo-llvm-cov",
    "cargo-crap",
    "cargo-deny",
    "cargo-sort",
    "cargo-acl",
    "cargo-machete",
    "cargo-udeps",
    "cargo-bsize",
    "typos",
    "cargo-mutest",
    "kani",
    "outpost",
];

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    const OURS: &str = "# Header.\n\n# Why A.\nA_VERSION=2\nB_VERSION=1\nD_VERSION=4\n\n# Which chock.\nCHOCK_VERSION=0.2.0\n";

    #[test]
    fn an_older_pin_file_takes_chocks_pins_and_keeps_the_projects_own_with_their_comments() {
        let held = "# Old header.\n\n# Old why A.\nA_VERSION=1\nOWN_REV=abc\nB_VERSION=1\n# Why.\nOWN_TOOLCHAIN=nightly\n# Loose.\n\n# Spaced.\nOWN_REPO=x\nCHOCK_VERSION=0.1.0\n# Trailing.\n";
        let moved = repinned(held, OURS).unwrap();
        assert_eq!(moved.from, "0.1.0");
        assert_eq!(
            moved.text,
            format!(
                "{OURS}\nOWN_REV=abc\n# Why.\nOWN_TOOLCHAIN=nightly\n\n# Spaced.\nOWN_REPO=x\n"
            )
        );
        let own = "kept as this project's own: OWN_REV, OWN_TOOLCHAIN, OWN_REPO";
        assert_eq!(
            moved.changes,
            [
                "A_VERSION 1 -> 2",
                "added D_VERSION=4",
                "CHOCK_VERSION 0.1.0 -> 0.2.0",
                own
            ]
        );
        let again = repinned(&moved.text, OURS).unwrap();
        assert_eq!(
            (again.text, again.changes),
            (moved.text, vec![own.to_string()])
        );
        let unspaced = repinned("OWN_REV=abc\nCHOCK_VERSION=0.1.0\n", OURS).unwrap();
        assert_eq!(unspaced.text, format!("{OURS}\nOWN_REV=abc\n"));
    }

    #[test]
    fn a_pin_file_naming_no_chock_or_holding_a_stray_line_is_not_moved() {
        let alone = repinned("CHOCK_VERSION=0.1.0\n", OURS).unwrap();
        assert_eq!(alone.text, OURS);
        assert!(
            !alone
                .changes
                .iter()
                .any(|change| change.starts_with("kept"))
        );
        assert_eq!(repinned("A_VERSION=1\n", OURS), None);
        assert_eq!(repinned("CHOCK_VERSION=0.1.0\nstray\n", OURS), None);
    }

    #[test]
    fn a_pin_naming_its_systems_is_of_no_use_elsewhere_and_one_naming_none_runs_anywhere() {
        let parsed = parse("CARGO_ACL_VERSION=0.9.0\nCARGO_ACL_OS=linux\nKANI_VERIFIER_VERSION=0.68.0\nKANI_VERIFIER_OS=\"linux, macos\"\nJUST_VERSION=1.58.0\n").unwrap();
        let (acl, kani, just) = (&parsed[0], &parsed[1], &parsed[2]);
        assert_eq!(acl.systems, Some(vec!["linux".to_string()]));
        assert_eq!(acl.elsewhere("linux"), None);
        assert_eq!(
            acl.elsewhere("macos").as_deref(),
            Some("runs only on linux, so macos has no use for it")
        );
        assert_eq!(
            kani.elsewhere("macos"),
            None,
            "the space after the comma is not a system"
        );
        assert_eq!(
            kani.elsewhere("windows").as_deref(),
            Some("runs only on linux, macos, so windows has no use for it")
        );
        assert_eq!(
            (just.systems.clone(), just.elsewhere("windows")),
            (None, None)
        );
    }

    #[test]
    fn a_systems_line_with_no_version_is_an_orphan_like_any_other_qualifier() {
        assert_eq!(
            parse("CARGO_ACL_OS=linux\n"),
            Err(ParseError::Orphan {
                line: 1,
                key: "CARGO_ACL".to_string(),
                suffix: OS_SUFFIX,
            })
        );
        assert_eq!(
            additional("CARGO_ACL_VERSION=0.9.0\nCARGO_ACL_OS=linux\n")
                .unwrap()
                .unchecked,
            Vec::<String>::new(),
            "a systems line is read, not left unchecked"
        );
    }

    fn pin(key: &str, crate_name: &str, command: &str, want: &str) -> Pin {
        Pin {
            key: key.to_string(),
            crate_name: crate_name.to_string(),
            command: command.to_string(),
            want: want.to_string(),
            setup: None,
            systems: None,
        }
    }

    #[test]
    fn a_version_key_becomes_a_pin_whose_crate_is_the_key_lowercased() {
        assert_eq!(
            parse("CARGO_NEXTEST_VERSION=0.9.143").unwrap(),
            vec![pin(
                "CARGO_NEXTEST_VERSION",
                "cargo-nextest",
                "cargo-nextest",
                "0.9.143"
            )]
        );
    }

    #[test]
    fn a_bin_override_changes_the_command_and_leaves_the_crate_alone() {
        assert_eq!(
            parse("ARBORIST_CLI_VERSION=0.2.1\nARBORIST_CLI_BIN=arborist").unwrap(),
            vec![pin(
                "ARBORIST_CLI_VERSION",
                "arborist-cli",
                "arborist",
                "0.2.1"
            )]
        );
    }

    #[test]
    fn pins_keep_the_order_the_file_wrote_them_in() {
        let got = parse("B_VERSION=2\nA_VERSION=1\nC_VERSION=3").unwrap();
        assert_eq!(
            got.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            ["B_VERSION", "A_VERSION", "C_VERSION"]
        );
    }

    /// A shell assigns `1.58.0` here. Refusing the line meant a pin file could not say why a
    /// version was chosen, which is the one thing a pin file is for.
    #[test]
    fn a_comment_after_a_value_is_not_part_of_the_value() {
        assert_eq!(
            parse("JUST_VERSION=1.58.0  # the build runner\n").unwrap(),
            vec![pin("JUST_VERSION", "just", "just", "1.58.0")]
        );
    }

    #[test]
    fn a_hash_inside_a_value_is_part_of_it() {
        assert_eq!(before_comment("A=a#b"), "A=a#b");
    }

    #[test]
    fn a_hash_inside_quotes_is_part_of_the_value() {
        assert_eq!(before_comment("A=\"a # b\""), "A=\"a # b\"");
    }

    /// Otherwise the `#` inside would start a comment and cut the value short.
    #[test]
    fn a_quote_of_the_other_kind_does_not_close_the_one_that_opened() {
        assert_eq!(before_comment("A=\"it's # fine\""), "A=\"it's # fine\"");
        assert_eq!(
            before_comment("A='say \"hi\" # now'"),
            "A='say \"hi\" # now'"
        );
    }

    #[test]
    fn a_comment_is_opened_by_what_precedes_the_hash_and_not_by_what_follows_it() {
        assert_eq!(
            before_comment("JUST_VERSION=1.58.0 #why"),
            "JUST_VERSION=1.58.0"
        );
        assert_eq!(before_comment("A=a# b"), "A=a# b");
    }

    /// The quote has to close, or a value holding one unbalanced quote would swallow the
    /// comment on every later line the parser reads as part of it.
    #[test]
    fn a_comment_after_a_closed_quote_is_still_a_comment() {
        assert_eq!(before_comment("A=\"one two\"  # why"), "A=\"one two\"");
    }

    #[test]
    fn a_line_that_is_only_a_comment_holds_no_assignment() {
        assert_eq!(before_comment("  # why this version"), "");
    }

    #[test]
    fn comments_and_blank_lines_contribute_nothing() {
        assert_eq!(
            parse("# a comment\n\n   \n  # indented\nJUST_VERSION=1.58.0\n").unwrap(),
            vec![pin("JUST_VERSION", "just", "just", "1.58.0")]
        );
    }

    #[test]
    fn a_non_version_key_is_carried_by_neither_pin_nor_error() {
        assert_eq!(
            parse("TOOL_REPO=https://example.invalid/x\nTOOL_BRANCH=main").unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_value_containing_an_equals_sign_is_kept_whole() {
        let got = parse("X_VERSION=1.0.0+build=2").unwrap();
        assert_eq!(got, vec![pin("X_VERSION", "x", "x", "1.0.0+build=2")]);
    }

    #[test]
    fn a_line_that_is_not_key_equals_value_names_its_line_number() {
        assert_eq!(
            parse("JUST_VERSION=1.58.0\nthis is not a pin\n"),
            Err(ParseError::Unreadable {
                line: 2,
                text: "this is not a pin".to_string()
            })
        );
    }

    #[test]
    fn a_version_key_with_no_value_is_an_error_rather_than_an_empty_pin() {
        assert_eq!(
            parse("JUST_VERSION=\n"),
            Err(ParseError::EmptyVersion {
                line: 1,
                key: "JUST_VERSION".to_string()
            })
        );
    }

    #[test]
    fn a_bin_override_naming_no_pin_is_an_error_rather_than_ignored() {
        assert_eq!(
            parse("JUST_VERSION=1.58.0\nARBORIST_CLI_BIN=arborist\n"),
            Err(ParseError::Orphan {
                line: 2,
                key: "ARBORIST_CLI".to_string(),
                suffix: BIN_SUFFIX
            })
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_the_key_or_the_version() {
        assert_eq!(
            parse("  JUST_VERSION = 1.58.0  ").unwrap(),
            vec![pin("JUST_VERSION", "just", "just", "1.58.0")]
        );
    }

    #[test]
    fn the_shipped_file_parses_to_the_tools_doctor_reports() {
        let got = parse(include_str!("../../tool-versions.env")).unwrap();
        let commands: Vec<&str> = got.iter().map(|p| p.command.as_str()).collect();
        assert_eq!(commands, SHIPPED);
    }

    #[test]
    fn an_unreadable_line_says_what_it_found_and_where() {
        let err = parse("oops\n").unwrap_err();
        assert_eq!(err.to_string(), "line 1: expected KEY=VALUE, found `oops`");
    }

    #[test]
    fn an_orphan_bin_error_names_the_version_key_it_wanted() {
        let err = parse("X_BIN=y\n").unwrap_err();
        assert_eq!(err.to_string(), "line 1: X_BIN has no matching X_VERSION");
    }

    #[test]
    fn a_value_with_spaces_and_no_quotes_is_refused_rather_than_parsed() {
        let err = parse("X_VERSION=1.0.0\nX_SETUP=cargo kani --version\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "line 2: X_SETUP has spaces in its value and no quotes; a shell sourcing this file \
             would run it instead of assigning it"
        );
    }

    /// The shortest thing that is still a quoted value, which is where the length check lives.
    #[test]
    fn a_value_that_is_only_its_quotes_unquotes_to_nothing() {
        assert_eq!(unquote("\"\""), "");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn a_quoted_value_is_stored_without_the_quotes() {
        let pins = parse("X_VERSION=1.0.0\nX_SETUP=\"cargo x --version\"\n").unwrap();
        assert_eq!(pins[0].setup.as_deref(), Some("cargo x --version"));
        let single = parse("X_VERSION=1.0.0\nX_SETUP='cargo x --version'\n").unwrap();
        assert_eq!(single[0].setup.as_deref(), Some("cargo x --version"));
    }

    #[test]
    fn an_orphan_setup_error_names_the_version_key_it_wanted() {
        let err = parse("X_SETUP=\"cargo x --version\"\n").unwrap_err();
        assert_eq!(err.to_string(), "line 1: X_SETUP has no matching X_VERSION");
    }

    #[test]
    fn a_tool_that_fetches_the_rest_of_itself_carries_the_command_that_does_it() {
        let pins =
            parse("KANI_VERIFIER_VERSION=0.68.0\nKANI_VERIFIER_SETUP=\"cargo kani --version\"\n")
                .unwrap();
        assert_eq!(pins[0].setup.as_deref(), Some("cargo kani --version"));
    }

    #[test]
    fn empty_pin_values_and_additional_comments_have_explicit_results() {
        let empty = parse("JUST_VERSION=\n").unwrap_err();
        assert_eq!(empty.to_string(), "line 1: JUST_VERSION pins no version");
        assert_eq!(
            additional("# only a comment\n\n").unwrap(),
            AdditionalPins::default()
        );
    }

    #[test]
    fn a_tool_with_no_setup_line_carries_none() {
        let pins = parse("JUST_VERSION=1.58.0\n").unwrap();
        assert_eq!(pins[0].setup, None);
    }

    #[test]
    fn nightly_pins_are_checked_without_becoming_installable_crates() {
        let text = "CARGO_MUTEST_VERSION=0.0.0\nMUTEST_NIGHTLY=nightly-2026-09-26\n";
        assert_eq!(
            parse(text).unwrap(),
            vec![pin(
                "CARGO_MUTEST_VERSION",
                "cargo-mutest",
                "cargo-mutest",
                "0.0.0"
            )]
        );
        assert_eq!(
            additional(text).unwrap(),
            AdditionalPins {
                toolchains: vec![ToolchainPin {
                    key: "MUTEST_NIGHTLY".to_string(),
                    want: "nightly-2026-09-26".to_string()
                }],
                unchecked: Vec::new(),
            }
        );
    }

    #[test]
    fn unknown_assignments_are_named_without_retaining_or_executing_their_values() {
        let text = "TOOL_VERSIONX=9\nTOOL_REPO=https://example.invalid/x\n";
        assert_eq!(
            additional(text).unwrap(),
            AdditionalPins {
                toolchains: Vec::new(),
                unchecked: vec![
                    "line 1: TOOL_VERSIONX".to_string(),
                    "line 2: TOOL_REPO".to_string()
                ],
            }
        );
        assert_eq!(parse(text).unwrap(), Vec::new());
    }

    #[test]
    fn a_nightly_pin_cannot_be_empty_or_supply_shell_syntax() {
        for value in [
            "",
            "stable",
            "nightly-",
            "../nightly",
            "nightly;false",
            "nightly\"",
            "\"nightly --version\"",
        ] {
            let error = additional(&format!("MUTEST_NIGHTLY={value}")).unwrap_err();
            assert_eq!(
                error,
                ParseError::InvalidToolchain {
                    line: 1,
                    key: "MUTEST_NIGHTLY".to_string()
                }
            );
            assert!(error.to_string().contains("must name a nightly toolchain"));
        }
        assert_eq!(
            additional("_NIGHTLY=nightly"),
            Err(ParseError::InvalidToolchain {
                line: 1,
                key: "_NIGHTLY".to_string()
            })
        );
    }

    #[test]
    fn nightly_toolchains_keep_host_names_and_pin_file_order() {
        let found =
            additional("# toolchain pins\nB_NIGHTLY=nightly\n\nA_NIGHTLY=nightly-2026-09-26-aarch64-apple-darwin")
                .unwrap();
        assert_eq!(
            found.toolchains,
            vec![
                ToolchainPin {
                    key: "B_NIGHTLY".to_string(),
                    want: "nightly".to_string()
                },
                ToolchainPin {
                    key: "A_NIGHTLY".to_string(),
                    want: "nightly-2026-09-26-aarch64-apple-darwin".to_string()
                },
            ]
        );
        assert_eq!(
            additional("bad line"),
            Err(ParseError::Unreadable {
                line: 1,
                text: "bad line".to_string()
            })
        );
    }
}
