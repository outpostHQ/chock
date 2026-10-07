//! What `init` sets up for coding agents: an editor hook that runs chock on every edit, and a
//! contract file saying how to read its answers.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::setup::init::{Error, held_or_empty, install_file, unwritable};

/// Adds chock's hook to `.claude/settings.json`, merged into any settings already there.
pub fn install_editor_hook(root: &Path) -> Result<String, Error> {
    let dir = root.join(".claude");
    let path = dir.join("settings.json");
    let merged = match planned(&path) {
        Hook::Already => return Ok(String::new()),
        Hook::Unreadable => {
            return Ok(
                "  note      .claude/settings.json is not JSON chock can read, so the editor \
                 hook was not added\n"
                    .to_string(),
            );
        }
        Hook::Merged(merged) => merged,
    };
    fs::create_dir_all(&dir).map_err(|e| unwritable(&dir, &e))?;
    crate::project::document::write(&path, &merged).map_err(|e| unwritable(&path, &e))?;
    Ok("  editor    .claude/settings.json runs chock on every edit\n  note      an editor session \
        already open may not fire it until it reloads\n"
        .to_string())
}

/// The outcome of adding the editor hook.
enum Hook {
    Merged(String),
    Already,
    Unreadable,
}

/// What the hook runs. `--hook` reads the editor's own JSON from stdin, so this needs no `jq`.
pub const ON_EDIT: &str = "chock edited --hook";

/// The outcome for the settings file at `path`. Only a missing file reads as empty, so an
/// unreadable one is never overwritten.
fn planned(path: &Path) -> Hook {
    match held_or_empty(path) {
        Ok(existing) => merge_editor_hook(&existing),
        Err(_) => Hook::Unreadable,
    }
}

/// Adds the `PostToolUse` entry to the settings text; settings it cannot read are left alone.
fn merge_editor_hook(existing: &str) -> Hook {
    if existing.contains(ON_EDIT) {
        return Hook::Already;
    }
    let mut settings: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(existing) {
            Ok(parsed) => parsed,
            Err(_) => return Hook::Unreadable,
        }
    };
    let entry = serde_json::json!({
        "matcher": "Write|Edit",
        "hooks": [{ "type": "command", "command": ON_EDIT }],
    });
    let Some(root) = settings.as_object_mut() else {
        return Hook::Unreadable;
    };
    let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return Hook::Unreadable;
    };
    let on_write = hooks
        .entry("PostToolUse")
        .or_insert_with(|| serde_json::json!([]));
    let Some(on_write) = on_write.as_array_mut() else {
        return Hook::Unreadable;
    };
    on_write.push(entry);
    match serde_json::to_string_pretty(&settings) {
        Ok(text) => Hook::Merged(format!("{text}\n")),
        Err(_) => Hook::Unreadable,
    }
}

/// How agents should call and read chock. Host instruction files point here instead of copying it.
const AGENT_FILE: &str = ".chock/agents.md";

const AGENT_CONTRACT: &str = include_str!("../../docs/agents.md");

/// The instruction files agent hosts read. Only those the project already has gain the pointer.
const HOST_FILES: [&str; 5] = [
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    ".github/copilot-instructions.md",
    ".cursor/rules/chock.mdc",
];

/// The line added to each host file.
fn pointer() -> String {
    format!("Quality gates: read {AGENT_FILE} before running or reading chock.")
}

/// Whether a host file lacks the pointer. Matches the path, not the line, so a reworded one counts.
fn needs_pointer(held: &str) -> bool {
    !held.contains(AGENT_FILE)
}

/// Writes the agent contract and appends a pointer to it in each host file the project has.
pub fn write_agent_contract(root: &Path) -> Result<String, Error> {
    let mut report = format!("{}\n", install_file(root, AGENT_FILE, AGENT_CONTRACT)?);
    let mut pointed = Vec::new();
    for name in HOST_FILES {
        let path = root.join(name);
        let Ok(held) = fs::read_to_string(&path) else {
            continue;
        };
        if !needs_pointer(&held) {
            continue;
        }
        let parted = if held.ends_with('\n') { "" } else { "\n" };
        let grown = format!("{held}{parted}\n{}\n", pointer());
        crate::project::document::write(&path, &grown).map_err(|e| unwritable(&path, &e))?;
        pointed.push(name);
    }
    if !pointed.is_empty() {
        let _ = writeln!(
            report,
            "  agent     {} point at {AGENT_FILE}",
            pointed.join(", ")
        );
    }
    Ok(report)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn merged(existing: &str) -> String {
        match merge_editor_hook(existing) {
            Hook::Merged(text) => text,
            Hook::Already => panic!("the hook was already there"),
            Hook::Unreadable => panic!("the settings could not be read"),
        }
    }

    #[test]
    fn the_editor_hook_is_added_to_settings_that_were_not_there() {
        let written = merged("");
        assert!(written.contains(ON_EDIT), "{written}");
        assert!(written.contains("Write|Edit"), "{written}");
    }

    #[test]
    fn settings_that_are_already_there_keep_what_they_hold() {
        let theirs = r#"{"model":"opus","hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[]}]}}"#;
        let written = merged(theirs);
        assert!(written.contains("\"model\""), "{written}");
        assert!(written.contains("\"Bash\""), "{written}");
        assert!(written.contains(ON_EDIT), "{written}");
    }

    #[test]
    fn running_init_twice_adds_the_hook_once() {
        let once = merged("");
        assert!(matches!(merge_editor_hook(&once), Hook::Already));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn settings_chock_cannot_read_are_left_untouched_and_reported() {
        assert!(matches!(merge_editor_hook("{not json"), Hook::Unreadable));
        let dir = crate::testdir::make("init-editor-unreadable");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(dir.join(".claude/settings.json"), "{not json").unwrap();
        let said = install_editor_hook(&dir).unwrap();
        assert!(said.contains("not JSON chock can read"), "{said}");
        assert_eq!(
            std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
            "{not json"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn settings_that_are_not_utf8_are_left_byte_for_byte_and_absent_ones_are_created() {
        let dir = crate::testdir::make("init-editor-not-utf8");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let path = dir.join(".claude/settings.json");
        let theirs = [b'{', 0xff, b'}'];
        std::fs::write(&path, theirs).unwrap();
        let said = install_editor_hook(&dir).unwrap();
        assert!(said.contains("not JSON chock can read"), "{said}");
        assert_eq!(std::fs::read(&path).unwrap(), theirs);
        std::fs::remove_file(&path).unwrap();
        install_editor_hook(&dir).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(ON_EDIT), "{written}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_contract_an_agent_needs_is_written_where_it_will_be_read() {
        let dir = crate::testdir::make("init-agents");
        write_agent_contract(&dir).unwrap();
        let held = fs::read_to_string(dir.join(AGENT_FILE)).unwrap();
        assert!(held.contains("`cannot_run` is not a pass"), "{held}");
        assert!(
            held.contains("Never move a baseline to make a gate pass"),
            "{held}"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_host_file_the_project_keeps_gains_a_pointer_and_one_it_does_not_is_not_created() {
        let dir = crate::testdir::make("init-agents-pointer");
        fs::write(dir.join("CLAUDE.md"), "# house rules\n").unwrap();
        write_agent_contract(&dir).unwrap();
        let claude = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(claude.starts_with("# house rules\n"), "{claude}");
        assert!(claude.contains(AGENT_FILE), "{claude}");
        assert!(!dir.join("GEMINI.md").exists());
    }

    /// What a `CLAUDE.md` holding `before` reads as once the contract is written beside it.
    fn pointed(name: &str, before: &str) -> String {
        let dir = crate::testdir::make(name);
        fs::write(dir.join("CLAUDE.md"), before).unwrap();
        write_agent_contract(&dir).unwrap();
        fs::read_to_string(dir.join("CLAUDE.md")).unwrap()
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_host_file_with_no_trailing_newline_still_gets_the_pointer_on_its_own_line() {
        let after = pointed("init-agents-no-newline", "# house rules");
        assert_eq!(after, format!("# house rules\n\n{}\n", pointer()));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_host_file_that_ends_in_a_newline_gains_exactly_one_blank_line_before_the_pointer() {
        let after = pointed("init-agents-newline", "# house rules\n");
        assert_eq!(after, format!("# house rules\n\n{}\n", pointer()));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_second_run_does_not_add_the_pointer_again() {
        let dir = crate::testdir::make("init-agents-twice");
        fs::write(dir.join("AGENTS.md"), "# house rules\n").unwrap();
        write_agent_contract(&dir).unwrap();
        let once = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        write_agent_contract(&dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("AGENTS.md")).unwrap(), once);
    }

    #[test]
    fn a_project_that_reworded_the_pointer_is_left_alone() {
        assert!(!needs_pointer("see .chock/agents.md for the gates"));
        assert!(needs_pointer("# house rules\n"));
    }
}
