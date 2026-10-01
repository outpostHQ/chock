//! The shape of each unpushed commit message, never its prose: subject width and ending, body
//! length, and a trailer crediting a tool.

use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "commits",
    about: "every commit not yet upstream has a subject that fits and a body that is not an essay",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

/// The widest subject that does not wrap in a terminal or most review interfaces.
pub const SUBJECT: usize = 72;

/// The most non-blank body lines; past this a message is usually recounting the diff.
pub const BODY: usize = 12;

/// The message limits a project allows; chock's numbers are only the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub subject: usize,
    pub body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            subject: SUBJECT,
            body: BODY,
        }
    }
}

impl Limits {
    /// A project that named one limit keeps chock's answer for the other.
    #[must_use]
    pub fn of(said: Option<crate::project::config::Message>) -> Self {
        let Some(said) = said else {
            return Self::default();
        };
        Self {
            subject: said.subject.unwrap_or(SUBJECT),
            body: said.body.unwrap_or(BODY),
        }
    }
}

/// A trailer that credits the tool rather than the person who decided.
const TOOL_TRAILERS: [&str; 3] = [
    "co-authored-by: claude",
    "generated with",
    "co-authored-by: ai",
];

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let messages = crate::project::vcs::unpushed(&ctx.root, ctx.vcs)?;
    let findings: Vec<Finding> = messages
        .iter()
        .flat_map(|(commit, message)| faults(commit, message, ctx.message))
        .collect();
    if findings.is_empty() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(findings))
}

/// A hash cut to eight characters; any other label is returned whole.
fn abbreviated(commit: &str) -> String {
    if commit.len() > 8 && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return commit.chars().take(8).collect();
    }
    commit.to_string()
}

#[must_use]
pub fn faults(commit: &str, message: &str, limits: Limits) -> Vec<Finding> {
    let short = abbreviated(commit);
    let at = |what: &str| Finding::at("", what).item(&short);
    let mut found = Vec::new();
    let mut lines = message.lines();
    let Some(subject) = lines.next() else {
        found.push(at("the message is empty"));
        return found;
    };
    if subject.chars().count() > limits.subject {
        found.push(at(&format!(
            "the subject is {} characters; past {} it wraps where it is read",
            subject.chars().count(),
            limits.subject
        )));
    }
    if subject.ends_with('.') {
        found.push(at(
            "the subject ends in a full stop, which reads as half a sentence",
        ));
    }
    let rest: Vec<&str> = lines.collect();
    if rest.first().is_some_and(|line| !line.trim().is_empty()) {
        found.push(at(
            "the body starts on the line below the subject, with no blank between",
        ));
    }
    let body = rest.iter().filter(|line| !line.trim().is_empty()).count();
    if body > limits.body {
        found.push(at(&format!(
            "the body is {body} lines; past {} it is recounting the diff, which the diff says",
            limits.body
        )));
    }
    let lowered = message.to_lowercase();
    if TOOL_TRAILERS.iter().any(|mark| lowered.contains(mark)) {
        found.push(at(
            "a trailer credits a tool; the person who decided is the author",
        ));
    }
    found
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_project_that_writes_longer_messages_sets_its_own_limits() {
        let theirs = Limits::of(Some(crate::project::config::Message {
            subject: Some(100),
            body: Some(60),
        }));
        let long = format!("{}\n\n{}", "x".repeat(90), "a line\n".repeat(40));
        assert_eq!(faults("abcdef1234", &long, theirs), Vec::<Finding>::new());
        let refused: Vec<String> = faults("abcdef1234", &long, Limits::default())
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            refused,
            vec![
                format!(
                    "abcdef12: the subject is 90 characters; past {SUBJECT} it wraps where it is read"
                ),
                format!(
                    "abcdef12: the body is 40 lines; past {BODY} it is recounting the diff, which the diff says"
                ),
            ]
        );
    }

    #[test]
    fn naming_one_limit_keeps_chocks_answer_for_the_other() {
        let wider = Limits::of(Some(crate::project::config::Message {
            subject: Some(100),
            body: None,
        }));
        assert_eq!(wider.subject, 100);
        assert_eq!(wider.body, BODY);
    }

    #[test]
    fn a_project_that_names_no_limits_gets_chocks_own() {
        assert_eq!(Limits::of(None), Limits::default());
        assert_eq!(Limits::default().subject, SUBJECT);
        assert_eq!(Limits::default().body, BODY);
    }

    fn rendered(message: &str) -> Vec<String> {
        faults("abcdef1234", message, Limits::default())
            .iter()
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn a_message_with_a_short_subject_and_a_short_body_is_clean() {
        assert_eq!(
            rendered(
                "Stop the walk descending into .outpost\n\nIt read the repository's own\nbookkeeping as source.\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_subject_wider_than_a_log_line_is_reported_with_its_width() {
        let long = "x".repeat(80);
        assert_eq!(
            rendered(&long),
            ["abcdef12: the subject is 80 characters; past 72 it wraps where it is read"]
        );
    }

    #[test]
    fn a_subject_at_exactly_the_width_is_allowed_and_one_past_it_is_not() {
        assert_eq!(rendered(&"x".repeat(SUBJECT)), Vec::<String>::new());
        assert_eq!(
            rendered(&"x".repeat(SUBJECT + 1)),
            [format!(
                "abcdef12: the subject is {} characters; past {SUBJECT} it wraps where it is read",
                SUBJECT + 1
            )]
        );
    }

    #[test]
    fn a_body_of_exactly_the_limit_is_not_reported() {
        let at_limit = format!("Fix the walk\n\n{}", "a reason\n".repeat(BODY));
        assert_eq!(rendered(&at_limit), Vec::<String>::new());
    }

    #[test]
    fn a_subject_ending_in_a_full_stop_is_reported() {
        assert_eq!(
            rendered("Fix the walk."),
            ["abcdef12: the subject ends in a full stop, which reads as half a sentence"]
        );
    }

    #[test]
    fn a_body_that_starts_without_a_blank_line_is_reported() {
        assert_eq!(
            rendered("Fix the walk\nIt descended into .outpost."),
            ["abcdef12: the body starts on the line below the subject, with no blank between"]
        );
    }

    #[test]
    fn a_body_past_the_limit_is_reported_with_its_length() {
        let essay = format!("Fix the walk\n\n{}", "a reason\n".repeat(13));
        assert_eq!(
            rendered(&essay),
            [
                "abcdef12: the body is 13 lines; past 12 it is recounting the diff, which the diff says"
            ]
        );
    }

    #[test]
    fn blank_lines_between_paragraphs_do_not_count_against_the_body() {
        let spaced = format!("Fix the walk\n\n{}", "a reason\n\n".repeat(10));
        assert_eq!(rendered(&spaced), Vec::<String>::new());
    }

    #[test]
    fn a_trailer_crediting_a_tool_is_reported_whatever_its_case() {
        assert_eq!(
            rendered("Fix the walk\n\nCo-Authored-By: Claude <noreply@anthropic.com>"),
            ["abcdef12: a trailer credits a tool; the person who decided is the author"]
        );
        assert_eq!(
            rendered("Fix the walk\n\n🤖 Generated with something").len(),
            1
        );
    }

    #[test]
    fn a_label_that_is_not_a_hash_is_shown_whole() {
        assert_eq!(abbreviated("abcdef1234567"), "abcdef12");
        assert_eq!(abbreviated("this message"), "this message");
    }

    #[test]
    fn an_empty_message_is_reported_rather_than_read_as_clean() {
        assert_eq!(rendered(""), ["abcdef12: the message is empty"]);
    }
}
