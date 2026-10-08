//! Moving a project's `tool-versions.env` to the chock that runs, which `init --local` does to a
//! pin file an older chock wrote.

use std::fmt;
use std::path::Path;

use crate::project;
use crate::setup::init::{Error, Written, held_or_empty, install_file, unwritable};
use crate::setup::{pins, version};

/// What `init --local` did to the pin file.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pinned {
    /// Written like any other file: it names no chock, or it is a link.
    File(Written),
    /// An older chock wrote it, and it now holds this chock's pins and the project's own.
    Updated(Vec<String>),
    /// It names a newer chock, so `init` leaves it as it is, and this says why.
    Kept(String),
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
            Self::Kept(why) => write!(f, "  kept      {name} — {why}"),
        }
    }
}

/// The pin file. One naming a chock no newer than this one takes this chock's pins and keeps the
/// project's own; one naming no chock, or a link, is written like any other file.
pub(super) fn install_pins(root: &Path, ours: &str) -> Result<Pinned, Error> {
    let path = root.join(project::PIN_FILE);
    let held = held_or_empty(&path)?;
    let Some(moved) = pins::repinned(&held, ours).filter(|_| !path.is_symlink()) else {
        return install_file(root, project::PIN_FILE, ours).map(Pinned::File);
    };
    let running = env!("CARGO_PKG_VERSION");
    if version::compare(&moved.from, running).is_none_or(std::cmp::Ordering::is_gt) {
        let from = moved.from;
        let why = format!("it pins chock {from}, and chock {running} updates only an older one's");
        return Ok(Pinned::Kept(why));
    }
    if moved.text == held {
        return Ok(Pinned::File(Written::Unchanged(project::PIN_FILE.into())));
    }
    crate::project::document::write(&path, &moved.text).map_err(|e| unwritable(&path, &e))?;
    Ok(Pinned::Updated(moved.changes))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::fs;

    use super::*;
    use crate::setup::init::local_pin_file;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_pin_file_an_older_chock_wrote_takes_this_chocks_pins_and_keeps_the_projects_own() {
        let (dir, running) = (
            crate::testdir::make("init-repin"),
            env!("CARGO_PKG_VERSION"),
        );
        let (ours, path) = (local_pin_file(running), dir.join(project::PIN_FILE));
        fs::write(
            &path,
            "CARGO_SORT_VERSION=0.1.0\nOWN_REV=abc\nCHOCK_VERSION=0.0.1\n",
        )
        .unwrap();
        let said = install_pins(&dir, &ours).unwrap().to_string();
        assert!(
            said.starts_with("  updated   tool-versions.env to this chock's pins"),
            "{said}"
        );
        assert!(
            said.contains(&format!("CHOCK_VERSION 0.0.1 -> {running}")),
            "{said}"
        );
        assert!(
            said.ends_with("kept as this project's own: OWN_REV"),
            "{said}"
        );
        let text = fs::read_to_string(&path).unwrap();
        let sort = ours
            .lines()
            .find(|line| line.starts_with("CARGO_SORT_VERSION="))
            .unwrap();
        assert!(
            text.starts_with(&format!("{sort}\nOWN_REV=abc\nCHOCK_VERSION={running}\n\n")),
            "{text}"
        );
        for pin in ours.lines().filter(|line| line.contains('=')) {
            assert!(
                text.lines().any(|line| line == pin),
                "{pin} missing: {text}"
            );
        }
        assert_eq!(
            install_pins(&dir, &ours).unwrap().to_string(),
            "  unchanged tool-versions.env"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_pin_file_naming_a_newer_chock_or_none_is_not_moved() {
        let (dir, running) = (
            crate::testdir::make("init-repin-kept"),
            env!("CARGO_PKG_VERSION"),
        );
        let (ours, path) = (local_pin_file(running), dir.join(project::PIN_FILE));
        for newer in ["99.0.0", "0.1.0-beta.1"] {
            let held = format!("CHOCK_VERSION={newer}\n");
            fs::write(&path, &held).unwrap();
            let why =
                format!("it pins chock {newer}, and chock {running} updates only an older one's");
            assert_eq!(
                install_pins(&dir, &ours).unwrap().to_string(),
                format!("  kept      tool-versions.env — {why}")
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), held);
        }
        fs::write(&path, "CARGO_SORT_VERSION=0.1.0\n").unwrap();
        assert_eq!(
            install_pins(&dir, &ours).unwrap(),
            Pinned::File(Written::Conflict {
                name: project::PIN_FILE.into(),
                kept: format!("{}.chock", project::PIN_FILE)
            })
        );
    }
}
