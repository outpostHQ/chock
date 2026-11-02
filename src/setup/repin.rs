//! Moving a project's `tool-versions.env` to the chock that runs, which `init --local` does to a
//! pin file an older chock wrote. No project pins chock: CI installs the newest.

use std::fmt;
use std::path::Path;

use crate::project;
use crate::setup::init::{Error, Written, held_or_empty, install_file, unwritable};
use crate::setup::pins;

/// What `init --local` did to the pin file.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pinned {
    /// Written like any other file: it shares no pin with chock's, or it is a link.
    File(Written),
    /// An older chock wrote it, and it now holds this chock's pins and the project's own.
    Updated(Vec<String>),
}

impl fmt::Display for Pinned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = project::PIN_FILE;
        match self {
            Self::File(written) => write!(f, "{written}"),
            Self::Updated(changes) => {
                write!(f, "  updated   {name} to this chock's pins")?;
                changes
                    .iter()
                    .try_for_each(|change| write!(f, "\n              {change}"))
            }
        }
    }
}

/// The pin file: one sharing a pin with `ours` takes our pins, keeps its own and drops any pin of
/// chock; chock's own copy stays; any other, or a link, is written like any file.
pub(super) fn install_pins(root: &Path, ours: &str) -> Result<Pinned, Error> {
    let path = root.join(project::PIN_FILE);
    let held = held_or_empty(&path)?;
    if !held.is_empty() && ours.starts_with(held.as_str()) {
        return Ok(Pinned::File(Written::Unchanged(project::PIN_FILE.into())));
    }
    let Some(moved) = pins::repinned(&held, ours).filter(|_| !path.is_symlink()) else {
        return install_file(root, project::PIN_FILE, ours).map(Pinned::File);
    };
    if moved.text == held {
        return Ok(Pinned::File(Written::Unchanged(project::PIN_FILE.into())));
    }
    crate::project::document::write(&path, &moved.text).map_err(|e| unwritable(&path, &e))?;
    Ok(Pinned::Updated(moved.changes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const OURS: &str = include_str!("../../tool-versions.env");

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn chocks_own_pin_file_is_left_as_it_is_with_no_copy_beside_it() {
        let dir = crate::testdir::make("init-repin-own");
        fs::write(dir.join(project::PIN_FILE), OURS).unwrap();
        let unchanged = Pinned::File(Written::Unchanged(project::PIN_FILE.into()));
        assert_eq!(install_pins(&dir, OURS).unwrap(), unchanged);
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [project::PIN_FILE]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_pin_file_an_older_chock_wrote_takes_this_chocks_pins_and_drops_its_chock_pin() {
        let dir = crate::testdir::make("init-repin");
        let path = dir.join(project::PIN_FILE);
        fs::write(
            &path,
            "CARGO_SORT_VERSION=0.1.0\nOWN_REV=abc\nCHOCK_VERSION=0.0.1\n",
        )
        .unwrap();
        let said = install_pins(&dir, OURS).unwrap().to_string();
        assert!(
            said.starts_with("  updated   tool-versions.env to this chock's pins"),
            "{said}"
        );
        let removed = "removed CHOCK_VERSION: CI installs the newest chock\n";
        assert!(
            said.ends_with(&format!(
                "{removed}              kept as this project's own: OWN_REV"
            )),
            "{said}"
        );
        let text = fs::read_to_string(&path).unwrap();
        let sort = OURS
            .lines()
            .find(|line| line.starts_with("CARGO_SORT_VERSION="))
            .unwrap();
        assert!(
            text.starts_with(&format!("{sort}\nOWN_REV=abc\n\n")),
            "{text}"
        );
        assert!(!text.contains("CHOCK_VERSION"), "{text}");
        for pin in OURS.lines().filter(|line| line.contains('=')) {
            assert!(
                text.lines().any(|line| line == pin),
                "{pin} missing: {text}"
            );
        }
        assert_eq!(
            install_pins(&dir, OURS).unwrap().to_string(),
            "  unchanged tool-versions.env"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_pin_file_sharing_no_pin_with_chocks_gets_a_copy_beside_it() {
        let dir = crate::testdir::make("init-repin-kept");
        fs::write(dir.join(project::PIN_FILE), "OWN_VERSION=1\n").unwrap();
        assert_eq!(
            install_pins(&dir, OURS).unwrap(),
            Pinned::File(Written::Conflict {
                name: project::PIN_FILE.into(),
                kept: format!("{}.chock", project::PIN_FILE)
            })
        );
    }
}
