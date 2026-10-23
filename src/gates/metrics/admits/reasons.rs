//! `.chock/escapes.json`: the escapes a person keeps, each listed by its file and its line, with
//! the reason it stays.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::run::report::Finding;

/// The file, from the root of the project.
pub(super) const FILE: &str = ".chock/escapes.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    text: String,
    reason: String,
}

/// Each kept escape as its file and its trimmed line, with the entries left that keep one.
#[derive(Debug, Default)]
pub(super) struct Kept(BTreeMap<(String, String), u64>);

impl Kept {
    /// What the file keeps, and nothing with no file. A file that does not parse, or an entry
    /// without a reason, leaves no escape that can be judged.
    pub(super) fn read(root: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(root.join(FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{FILE}: {e}")),
        };
        let entries: Vec<Entry> =
            serde_json::from_str(&text).map_err(|e| format!("{FILE}: {e}"))?;
        let mut kept = Self::default();
        for Entry { path, text, reason } in entries {
            if reason.trim().is_empty() {
                return Err(format!(
                    "{FILE}: the entry for `{path}` that keeps `{text}` gives no reason; write why \
                     the escape stays"
                ));
            }
            *kept.0.entry((path, text.trim().to_string())).or_default() += 1;
        }
        Ok(kept)
    }

    /// Whether an entry keeps the escape on `line` of `path`. Each entry keeps one escape.
    pub(super) fn take(&mut self, path: &str, line: &str) -> bool {
        match self.0.get_mut(&(path.to_string(), line.trim().to_string())) {
            Some(left) if *left > 0 => {
                *left -= 1;
                true
            }
            _ => false,
        }
    }

    /// Each entry that kept no escape: its escape is gone, or its line reads otherwise now.
    pub(super) fn unused(self) -> Vec<Finding> {
        let left = self.0.into_iter().filter(|(_, left)| *left > 0);
        let fault = |((path, text), left): ((String, String), u64)| {
            let message = format!(
                "{left} entry(s) for `{path}` keep `{text}`, and no escape there is left for them; \
                 take each one out, or copy the escape's line again"
            );
            Finding::at(FILE, &message)
        };
        left.map(fault).collect()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn kept(json: &str) -> Result<Kept, String> {
        let held = crate::testdir::Held::tree("gate-admits-reasons", &[(FILE, json)]);
        Kept::read(&held.root)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_entry_keeps_one_escape_and_one_left_over_is_a_fault() {
        let json = r#"[
            {"path": "a.rs", "text": "  kani::assume(x < 3);", "reason": "bounds the input"},
            {"path": "a.rs", "text": "kani::assume(x < 3);", "reason": "bounds it again"},
            {"path": "b.lean", "text": "axiom c : False", "reason": "gone since"}
        ]"#;
        let mut kept = kept(json).unwrap();
        assert!(kept.take("a.rs", "    kani::assume(x < 3);"));
        assert!(kept.take("a.rs", "kani::assume(x < 3);"));
        assert!(!kept.take("a.rs", "kani::assume(x < 3);"));
        assert!(!kept.take("b.rs", "axiom c : False"));
        let left = "1 entry(s) for `b.lean` keep `axiom c : False`, and no escape there is left for \
                    them; take each one out, or copy the escape's line again";
        assert_eq!(kept.unused(), [Finding::at(FILE, left)]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn no_file_keeps_nothing_and_a_bad_file_or_a_blank_reason_judges_nothing() {
        let none = crate::testdir::Held::tree("gate-admits-no-reasons", &[("a.rs", "")]);
        assert!(Kept::read(&none.root).unwrap().unused().is_empty());
        let blank = r#"[{"path": "a.rs", "text": "x", "reason": " "}]"#;
        assert!(kept(blank).unwrap_err().contains("gives no reason"));
        let unknown = r#"[{"path": "a.rs", "text": "x", "reason": "r", "why": "w"}]"#;
        assert!(kept(unknown).unwrap_err().contains("unknown field `why`"));
        assert!(kept("{").unwrap_err().starts_with(FILE));
        let folder = crate::testdir::Held::tree(
            "gate-admits-reasons-folder",
            &[(".chock/escapes.json/a", "")],
        );
        assert!(Kept::read(&folder.root).unwrap_err().starts_with(FILE));
    }
}
