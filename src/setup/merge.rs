//! Adds the keys chock ships to a TOML file the project already holds. No line of the project's
//! changes, and a key the project sets keeps the project's value.

use std::fs;
use std::path::Path;

use crate::setup::init::{Error, install_file, unwritable};

/// One `key = value`: its name, its first line with the comment lines above it, and the line after
/// its last.
#[derive(Debug)]
struct Entry {
    name: String,
    from: usize,
    to: usize,
}

/// One table: its header's lines with the comments above, and the line after its last key.
#[derive(Debug, Default)]
struct Table {
    name: String,
    from: usize,
    head: usize,
    end: usize,
    keys: Vec<Entry>,
}

/// A file's tables in order. The first is the root table, which has no header.
#[derive(Debug)]
struct Layout {
    tables: Vec<Table>,
}

impl Layout {
    /// Whether table `table` holds `key` as a key of its own.
    fn holds(&self, table: &str, key: &str) -> bool {
        self.tables
            .iter()
            .any(|it| it.name == table && it.keys.iter().any(|held| held.name == key))
    }

    /// Whether the file sets `key` of `table`: as a key, or as a table `[table.key]` or below it.
    fn sets(&self, table: &str, key: &str) -> bool {
        let path = joined(table, key);
        let below = format!("{path}.");
        self.holds(table, key)
            || (self.tables.iter()).any(|it| it.name == path || it.name.starts_with(&below))
    }

    /// Whether the file writes `table` as a key of its parent, as in `licenses = { .. }`.
    fn inlines(&self, table: &str) -> bool {
        let (parent, leaf) = table.rsplit_once('.').unwrap_or(("", table));
        !table.is_empty() && self.holds(parent, leaf)
    }
}

/// `table.key`, or the key alone in the root table.
fn joined(table: &str, key: &str) -> String {
    if table.is_empty() {
        key.to_string()
    } else {
        format!("{table}.{key}")
    }
}

/// How many lists and inline tables the line leaves open, less those it closes. Text in quotes
/// and after `#` does not count.
fn nesting(code: &str) -> i32 {
    let mut depth = 0;
    let mut quote = None;
    let mut before = ' ';
    for c in code.chars() {
        match quote {
            Some(open) if c == open && before != '\\' => quote = None,
            Some(_) => {}
            None if c == '#' => break,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '[' || c == '{' => depth += 1,
            None if c == ']' || c == '}' => depth -= 1,
            None => {}
        }
        before = c;
    }
    depth
}

/// The table a `[name]` or `[[name]]` line opens.
fn header(code: &str) -> Option<&str> {
    let rest = code.strip_prefix('[')?.trim_start_matches('[');
    rest.split_once(']').map(|(name, _)| name.trim())
}

/// The key a `key = value` line sets: the first word of a dotted key, without its quotes.
fn key(code: &str) -> Option<String> {
    let (name, _) = code.split_once('=')?;
    let first = name.split('.').next().unwrap_or(name);
    Some(first.trim().trim_matches(['"', '\'']).to_string())
}

/// The tables and keys of a TOML text. A list or inline table left open takes the lines that
/// follow until it closes, and the comment lines right above a key or header go with it.
fn layout(text: &str) -> Layout {
    let (mut tables, mut table) = (Vec::new(), Table::default());
    let mut lead = None;
    let mut depth = 0;
    for (at, line) in text.lines().enumerate() {
        let code = line.trim();
        if depth > 0 {
            depth += nesting(code);
            table.end = at + 1;
            if let Some(open) = table.keys.last_mut() {
                open.to = at + 1;
            }
        } else if code.is_empty() {
            lead = None;
        } else if code.starts_with('#') {
            lead.get_or_insert(at);
        } else if let Some(name) = header(code) {
            let next = Table {
                name: name.to_string(),
                from: lead.take().unwrap_or(at),
                head: at + 1,
                end: at + 1,
                keys: Vec::new(),
            };
            tables.push(std::mem::replace(&mut table, next));
        } else if let Some(name) = key(code) {
            depth = nesting(code);
            table.end = at + 1;
            table.keys.push(Entry {
                name,
                from: lead.take().unwrap_or(at),
                to: at + 1,
            });
        }
    }
    tables.push(table);
    Layout { tables }
}

/// Lines `from..to` of `lines`, each with its line end.
fn text(lines: &[&str], from: usize, to: usize) -> String {
    let part = lines.get(from..to).unwrap_or_default();
    part.iter().map(|line| format!("{line}\n")).collect()
}

/// `held` with every key of `shipped` it does not set, and the name of each key added. A key goes
/// after its table's last key; a table the project lacks goes at the end.
#[must_use]
pub fn merged(held: &str, shipped: &str) -> (String, Vec<String>) {
    let (ours, theirs) = (layout(shipped), layout(held));
    let lines: Vec<&str> = shipped.lines().collect();
    let mut inserts: Vec<(usize, String)> = Vec::new();
    let mut tail = String::new();
    let mut added = Vec::new();
    for table in ours.tables.iter().filter(|it| !theirs.inlines(&it.name)) {
        let missing = (table.keys.iter()).filter(|it| !theirs.sets(&table.name, &it.name));
        let block: String = missing
            .map(|it| {
                added.push(joined(&table.name, &it.name));
                text(&lines, it.from, it.to)
            })
            .collect();
        if block.is_empty() {
            continue;
        }
        match theirs.tables.iter().find(|it| it.name == table.name) {
            Some(found) => inserts.push((found.end, block)),
            None => {
                tail.push('\n');
                tail.push_str(&text(&lines, table.from, table.head));
                tail.push_str(&block);
            }
        }
    }
    (woven(held, &inserts, &tail), added)
}

/// `held` with each insert before the line it names, and `tail` after the last line.
fn woven(held: &str, inserts: &[(usize, String)], tail: &str) -> String {
    let mut out = String::new();
    let lines = held.split_inclusive('\n');
    for (at, line) in lines.clone().enumerate() {
        add_at(&mut out, inserts, at);
        out.push_str(line);
        out.push_str(if line.ends_with('\n') { "" } else { "\n" });
    }
    add_at(&mut out, inserts, lines.count());
    out + tail
}

fn add_at(out: &mut String, inserts: &[(usize, String)], at: usize) {
    for (_, block) in inserts.iter().filter(|(line, _)| *line == at) {
        out.push_str(block);
    }
}

/// Brings the project's `name` up to what chock ships, and says what happened in one line. A
/// missing file is written; a held file gains the keys it lacks; a symlink is left as it is.
pub fn install(root: &Path, name: &str, shipped: &str) -> Result<String, Error> {
    let path = root.join(name);
    let kept = || format!("  kept      {name} is a symlink, so chock leaves it");
    let brought = || -> Result<String, Error> {
        let Ok(held) = fs::read_to_string(&path) else {
            return Ok(install_file(root, name, shipped)?.to_string());
        };
        let (grown, added) = merged(&held, shipped);
        if added.is_empty() {
            return Ok(format!("  unchanged {name}"));
        }
        crate::project::document::write(&path, &grown).map_err(|e| unwritable(&path, &e))?;
        Ok(format!(
            "  merged    {name} — added {}; every line of yours stays",
            added.join(", ")
        ))
    };
    path.is_symlink().then(kept).map_or_else(brought, Ok)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const SHIPPED: &str = "[advisories]\nyanked = \"deny\"\n\n[licenses]\n# Why private.\n\
        private = { ignore = true }\n\n# The list.\nallow = [\n    \"MIT\",\n    \"ISC\",\n]\n\n\
        [sources]\nunknown-git = \"deny\"\n";

    #[test]
    fn a_file_that_sets_every_shipped_key_is_returned_as_it_is() {
        let (text, added) = merged(SHIPPED, SHIPPED);
        assert_eq!(text, SHIPPED);
        assert!(added.is_empty());
    }

    #[test]
    fn a_missing_key_goes_after_the_last_key_of_its_table_with_its_comment() {
        let held = "[licenses]\nallow = [\n    \"MIT\",\n]\n\n[graph]\ntargets = []\n\n\
                    [advisories]\nyanked = \"warn\"\n[sources]\nunknown-git = \"deny\"\n";
        let (text, added) = merged(held, SHIPPED);
        assert_eq!(added, ["licenses.private"]);
        assert_eq!(
            text,
            "[licenses]\nallow = [\n    \"MIT\",\n]\n# Why private.\nprivate = { ignore = true }\n\
             \n[graph]\ntargets = []\n\n[advisories]\nyanked = \"warn\"\n[sources]\n\
             unknown-git = \"deny\"\n"
        );
    }

    #[test]
    fn a_missing_table_goes_at_the_end_with_its_keys_and_their_comments() {
        let held = "[advisories]\nyanked = \"deny\"\n[sources]\nunknown-git = \"deny\"";
        let (text, added) = merged(held, SHIPPED);
        assert_eq!(added, ["licenses.private", "licenses.allow"]);
        assert_eq!(
            text,
            "[advisories]\nyanked = \"deny\"\n[sources]\nunknown-git = \"deny\"\n\n[licenses]\n\
             # Why private.\nprivate = { ignore = true }\n# The list.\nallow = [\n    \"MIT\",\n    \
             \"ISC\",\n]\n"
        );
    }

    #[test]
    fn a_key_the_project_writes_as_its_own_table_is_not_added_again() {
        let held = "[advisories]\nyanked = \"deny\"\n[licenses]\nallow = []\n\
                    [licenses.private]\nignore = false\n[sources]\nunknown-git = \"deny\"\n";
        let (text, added) = merged(held, SHIPPED);
        assert!(added.is_empty(), "{added:?}");
        assert_eq!(text, held);
    }

    #[test]
    fn a_table_below_a_key_counts_as_that_key_but_a_longer_name_does_not() {
        let below = layout("[licenses.private.more]\nx = 1\n");
        assert!(below.sets("licenses", "private"));
        let longer = layout("[licenses.privateer]\nx = 1\n");
        assert!(!longer.sets("licenses", "private"));
    }

    #[test]
    fn a_table_the_project_writes_inline_at_the_root_is_left_alone() {
        let held = "advisories = { yanked = \"warn\" }\nlicenses.allow = [\"MIT\"]\n\
                    [sources]\nunknown-git = \"deny\"\n";
        let (text, added) = merged(held, SHIPPED);
        assert!(added.is_empty(), "{added:?}");
        assert_eq!(text, held);
    }

    #[test]
    fn a_root_key_is_added_before_the_first_table_and_named_without_a_dot() {
        let (text, added) = merged("[a]\nb = 1\n", "top = 1\n[a]\nb = 2\n");
        assert_eq!(added, ["top"]);
        assert_eq!(text, "top = 1\n[a]\nb = 1\n");
    }

    #[test]
    fn a_table_with_no_key_takes_the_new_key_right_after_its_header() {
        let (text, added) = merged("[a]\n\n[b]\nc = 1\n", "[a]\nk = 1\n");
        assert_eq!(added, ["a.k"]);
        assert_eq!(text, "[a]\nk = 1\n\n[b]\nc = 1\n");
    }

    #[test]
    fn brackets_in_a_string_or_after_a_hash_do_not_open_a_list() {
        assert_eq!(nesting("a = [ { x = 1 }"), 1);
        assert_eq!(nesting("]"), -1);
        assert_eq!(nesting("a = \"[\" # ["), 0);
        assert_eq!(nesting("a = '{'"), 0);
        assert_eq!(nesting("a = \"\\\"[\""), 0);
        assert_eq!(nesting("a = \"it's\" # {"), 0);
    }

    #[test]
    fn a_list_left_open_takes_the_lines_up_to_its_close_and_the_next_key_is_its_own() {
        let read = layout("[t]\na = [\n  \"x = 1\",\n  [1],\n]\nb = 2\n");
        let table = read.tables.last().unwrap();
        let spans: Vec<_> = (table.keys.iter())
            .map(|it| (it.name.as_str(), it.from, it.to))
            .collect();
        assert_eq!(spans, [("a", 1, 5), ("b", 5, 6)]);
        assert_eq!(table.end, 6);
    }

    #[test]
    fn a_blank_line_parts_a_comment_from_the_key_below_it() {
        let read = layout("# About the file.\n\n# About a.\na = 1\n[[t.u]]\n\"b\".c = 2\n");
        let root = read.tables.first().unwrap();
        assert_eq!((root.keys[0].from, root.keys[0].to), (2, 4));
        let table = read.tables.last().unwrap();
        assert_eq!(table.name, "t.u");
        assert_eq!((table.from, table.head), (4, 5));
        assert_eq!(table.keys[0].name, "b");
    }

    #[test]
    fn a_comment_above_a_header_goes_with_the_table_it_heads() {
        let (text, _) = merged("", "# About t.\n[t]\na = 1\n");
        assert_eq!(text, "\n# About t.\n[t]\na = 1\n");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_missing_file_is_written_and_a_second_run_changes_nothing() {
        let dir = crate::testdir::make("merge-new");
        let first = install(&dir, "deny.toml", SHIPPED).unwrap();
        assert_eq!(first, "  created   deny.toml");
        let second = install(&dir, "deny.toml", SHIPPED).unwrap();
        assert_eq!(second, "  unchanged deny.toml");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_held_file_gains_the_keys_and_the_line_names_them() {
        let dir = crate::testdir::make("merge-held");
        let path = dir.join("deny.toml");
        fs::write(&path, "[licenses]\nallow = []\n").unwrap();
        let line = install(&dir, "deny.toml", SHIPPED).unwrap();
        assert_eq!(
            line,
            "  merged    deny.toml — added advisories.yanked, licenses.private, \
             sources.unknown-git; every line of yours stays"
        );
        let grown = fs::read_to_string(&path).unwrap();
        assert!(grown.starts_with("[licenses]\nallow = []\n# Why private.\nprivate"));
        assert!(!dir.join("deny.toml.chock").exists());
        let again = install(&dir, "deny.toml", SHIPPED).unwrap();
        assert_eq!(again, "  unchanged deny.toml");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_left_as_it_is() {
        let dir = crate::testdir::make("merge-link");
        let target = dir.join("elsewhere.toml");
        fs::write(&target, "[licenses]\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("deny.toml")).unwrap();
        let line = install(&dir, "deny.toml", SHIPPED).unwrap();
        assert_eq!(
            line,
            "  kept      deny.toml is a symlink, so chock leaves it"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "[licenses]\n");
    }
}
