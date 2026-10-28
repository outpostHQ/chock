//! Tracked files that steer git itself on a clone or a checkout: a `.gitmodules` value that git
//! reads other than as it shows, and a link into a `.git` directory.

use std::path::Path;

use crate::run::report::Finding;

/// The rule a `.gitmodules` line breaks, as the finding's item.
pub(super) const MODULES: &str = "gitmodules";

/// The rule a link into `.git` breaks, as the finding's item.
pub(super) const LINK: &str = "git_link";

/// Each `.gitmodules` line with a control character, which git can drop so a path names another
/// place (CVE-2025-48384), or a value that begins with `-`, an option to git (CVE-2017-1000117).
pub(super) fn modules(path: &str, text: &str) -> Vec<Finding> {
    // `lines` takes off the one carriage return of each Windows line end.
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let fault = if line.chars().any(|c| c.is_control() && c != '\t') {
                "holds a control character, which git can drop and so read another path: delete it"
            } else if value(line).is_some_and(|value| value.starts_with('-')) {
                "holds a value that begins with `-`, which git reads as an option: rename it"
            } else {
                return None;
            };
            let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
            Some(Finding::at(path, fault).line(line).item(MODULES))
        })
        .collect()
}

/// The value of a `key = value` line, without its opening quote.
fn value(line: &str) -> Option<&str> {
    let (_, value) = line.split_once('=')?;
    Some(value.trim().trim_start_matches('"'))
}

/// A tracked link whose target passes through a `.git` directory, in any case: a checkout that
/// follows it writes git's own files, as hooks (CVE-2024-32002).
pub(super) fn link(path: &str, target: &Path) -> Option<Finding> {
    let into_git = target
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"));
    let message = "a tracked link into `.git`, so a checkout can write git's own files: delete it";
    into_git.then(|| Finding::at(path, message).item(LINK))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str) -> Vec<u32> {
        modules(".gitmodules", text)
            .iter()
            .filter_map(|finding| finding.line)
            .collect()
    }

    #[test]
    fn a_carriage_return_inside_a_value_or_a_value_that_begins_with_a_dash_is_named() {
        let text = "[submodule \"a\"]\r\n\tpath = \"a\r\"\r\n\turl = -upload-pack=touch\n\
                    [submodule \"b\"]\n\tpath = b\n\turl = https://example.com/b-c.git\n";
        assert_eq!(lines_of(text), [2, 3]);
    }

    #[test]
    fn windows_line_ends_tabs_and_inner_dashes_pass() {
        let text = "[submodule \"x-y\"]\r\n\tpath = libs/x-y\r\n\turl = ../x-y.git\r\n";
        assert_eq!(lines_of(text), Vec::<u32>::new());
    }

    #[test]
    fn a_link_into_git_in_any_case_is_named_and_one_beside_it_is_not() {
        for target in [".git/hooks", "../.git", "sub/.GIT/config"] {
            assert!(link("a", Path::new(target)).is_some(), "{target}");
        }
        for target in ["docs/README.md", ".github/workflows", "x.git"] {
            assert!(link("a", Path::new(target)).is_none(), "{target}");
        }
    }
}
