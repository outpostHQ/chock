//! Reading and writing chock's versioned JSON documents. `write` is the atomic writer every file
//! chock produces goes through.

use std::fmt;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;

/// A JSON document chock owns, versioned so an older chock refuses a newer file rather than
/// half-reading it. The schema is bumped only by adding a field.
pub trait Versioned: DeserializeOwned {
    /// The version this chock writes and the highest it can read.
    const SCHEMA: u32;
    /// Where it lives, relative to the project root.
    const FILE: &'static str;

    /// The version the parsed document declares.
    fn version(&self) -> u32;

    /// What the document is, for a message a reader can act on.
    fn describe() -> &'static str;
}

/// Why a document could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Unreadable {
        path: String,
        reason: String,
    },
    Unparsable {
        path: String,
        reason: String,
        expected: &'static str,
    },
    /// Written by a chock whose format this one predates, so its numbers may not mean the same.
    FromTheFuture {
        path: String,
        found: u32,
        reads: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, reason } => write!(f, "cannot read {path}: {reason}"),
            Self::Unparsable {
                path,
                reason,
                expected,
            } => write!(f, "{path} is not {expected}: {reason}"),
            Self::FromTheFuture { path, found, reads } => write!(
                f,
                "{path} is schema v{found}; this chock reads v{reads}. Upgrade chock."
            ),
        }
    }
}

/// Where the published schemas live; a test checks each named file is in this repository.
const SCHEMAS: &str = "https://raw.githubusercontent.com/outpostHQ/chock/main/schema";

#[must_use]
pub fn schema_url(document: &str, version: u32) -> String {
    format!("{SCHEMAS}/{document}-v{version}.json")
}

pub fn parse<T: Versioned>(text: &str, path: &str) -> Result<T, Error> {
    let parsed: T = serde_json::from_str(text).map_err(|e| Error::Unparsable {
        path: path.to_string(),
        reason: e.to_string(),
        expected: T::describe(),
    })?;
    if parsed.version() > T::SCHEMA {
        return Err(Error::FromTheFuture {
            path: path.to_string(),
            found: parsed.version(),
            reads: T::SCHEMA,
        });
    }
    Ok(parsed)
}

/// Reads the document under `root`; `Ok(None)` when there is no file.
pub fn read<T: Versioned>(root: &Path) -> Result<Option<T>, Error> {
    let path = root.join(T::FILE);
    let shown = path.display().to_string();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse::<T>(&text, &shown).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Unreadable {
            path: shown,
            reason: e.to_string(),
        }),
    }
}

/// Refuses a relative path: every path chock writes hangs off an absolute project root.
pub fn rooted(path: &Path) -> std::io::Result<()> {
    if path.is_absolute() {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} is not under a project root", path.display()),
    ))
}

/// Writes atomically, and only to an absolute path.
pub fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    rooted(path).and_then(|()| staged(path, contents))
}

/// Writes a temporary beside the target and renames it over, so the target is never torn.
fn staged(path: &Path, contents: &str) -> std::io::Result<()> {
    let beside = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("document");
    // The pid keeps two chock runs in one tree from writing the same temporary file.
    let temp = beside.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, contents)?;
    // The caller never learns the temporary's name, so a failed rename removes it here.
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Pretty-printed with a trailing newline, since the documents are read in diffs.
pub fn render<T: serde::Serialize>(document: &T) -> String {
    let mut text = serde_json::to_string_pretty(document).unwrap_or_default();
    text.push('\n');
    text
}

/// `now` written over the file it was read from, so the diff shows the change and not chock's
/// order. A key keeps its place and spelling unless its value changed; a key unknown to `T` stays.
pub fn rewrite<T: serde::Serialize + DeserializeOwned>(original: &str, now: &T) -> String {
    let read = (
        serde_json::from_str::<Entries>(original),
        serde_json::from_str::<T>(original).and_then(serde_json::to_value),
        serde_json::to_value(now),
    );
    let (Ok(Entries(entries)), Ok(Value::Object(before)), Ok(Value::Object(after))) = read else {
        return render(now);
    };
    let indent = indent_of(original);
    let kept = entries
        .iter()
        .filter_map(|(key, raw)| match (before.get(key), after.get(key)) {
            (Some(_), None) => None,
            (was, Some(value)) if was != Some(value) => Some((key, pretty(value, indent))),
            _ => Some((key, raw.get().to_string())),
        });
    let added = after
        .iter()
        .filter(|(key, value)| {
            before.get(*key) != Some(value) && !entries.iter().any(|(had, _)| had == *key)
        })
        .map(|(key, value)| (key, pretty(value, indent)));
    let lines: Vec<String> = kept
        .chain(added)
        .map(|(key, value)| format!("{indent}{}: {value}", Value::from(key.as_str())))
        .collect();
    format!("{{\n{}\n}}\n", lines.join(",\n"))
}

/// A JSON object's entries in the file's order, each value exactly as written.
struct Entries(Vec<(String, Box<serde_json::value::RawValue>)>);

impl<'de> serde::Deserialize<'de> for Entries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(InOrder)
    }
}

struct InOrder;

impl<'de> serde::de::Visitor<'de> for InOrder {
    type Value = Entries;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Entries, M::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry()? {
            entries.push(entry);
        }
        Ok(Entries(entries))
    }
}

/// The indentation of the file's first key, so what chock writes lines up with what it kept.
fn indent_of(original: &str) -> &str {
    original
        .lines()
        .skip(1)
        .find_map(|line| {
            let key = line.trim_start();
            key.starts_with('"')
                .then(|| line.strip_suffix(key))
                .flatten()
        })
        .unwrap_or("  ")
}

/// One value in the file's indentation, starting at the depth of a top-level key.
fn pretty(value: &Value, indent: &str) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let written = serde::Serialize::serialize(
        value,
        &mut serde_json::Serializer::with_formatter(&mut out, formatter),
    );
    let text = written
        .ok()
        .and_then(|()| String::from_utf8(out).ok())
        .unwrap_or_default();
    text.replace('\n', &format!("\n{indent}"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    fn temporaries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn a_document_is_written_whole_and_leaves_no_temporary_behind() {
        let dir = crate::testdir::make("document-atomic");
        let path = dir.join("baseline.json");
        write(&path, "{\"version\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\":1}");
        assert_eq!(temporaries(&dir), Vec::<String>::new());
    }

    #[test]
    fn a_rename_that_could_not_happen_takes_its_temporary_with_it() {
        let dir = crate::testdir::make("document-rename-refused");
        let occupied = dir.join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        assert!(write(&occupied, "{}").is_err());
        assert_eq!(temporaries(&dir), Vec::<String>::new());
    }

    #[test]
    fn writing_again_replaces_what_was_there() {
        let dir = crate::testdir::make("document-replace");
        let path = dir.join("baseline.json");
        write(&path, "first").unwrap();
        write(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn a_directory_that_does_not_exist_is_an_error_rather_than_a_silent_loss() {
        let dir = crate::testdir::make("document-absent");
        assert!(write(&dir.join("no/such/dir/x.json"), "{}").is_err());
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Probe {
        version: u32,
        held: String,
    }

    impl Versioned for Probe {
        const SCHEMA: u32 = 2;
        const FILE: &'static str = ".chock/probe.json";
        fn version(&self) -> u32 {
            self.version
        }
        fn describe() -> &'static str {
            "a probe"
        }
    }

    #[test]
    fn a_document_at_the_schema_this_chock_writes_is_read() {
        let probe: Probe = parse(r#"{"version":2,"held":"x"}"#, "p.json").unwrap();
        assert_eq!(probe.held, "x");
    }

    #[test]
    fn a_document_from_an_older_schema_is_still_read() {
        let probe: Probe = parse(r#"{"version":1,"held":"x"}"#, "p.json").unwrap();
        assert_eq!(probe.version, 1);
    }

    #[test]
    fn a_document_from_a_newer_chock_is_refused_rather_than_half_read() {
        let err = parse::<Probe>(r#"{"version":9,"held":"x"}"#, "p.json").unwrap_err();
        assert_eq!(
            err,
            Error::FromTheFuture {
                path: "p.json".to_string(),
                found: 9,
                reads: 2
            }
        );
        assert_eq!(
            err.to_string(),
            "p.json is schema v9; this chock reads v2. Upgrade chock."
        );
    }

    #[test]
    fn text_that_is_not_the_document_names_the_file_and_what_was_wanted() {
        let err = parse::<Probe>("not json", "p.json").unwrap_err();
        assert!(matches!(&err, Error::Unparsable { path, expected, .. }
            if path == "p.json" && *expected == "a probe"));
        assert!(err.to_string().starts_with("p.json is not a probe:"));
    }

    #[test]
    fn a_missing_file_is_absence_rather_than_failure() {
        let dir = crate::testdir::make("document-missing");
        assert_eq!(read::<Probe>(&dir), Ok(None));
    }

    #[derive(Serialize, Deserialize)]
    struct Shaped {
        version: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        list: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    }

    fn shaped(list: &[&str], note: Option<&str>) -> Shaped {
        Shaped {
            version: 1,
            list: list.iter().map(|item| (*item).to_string()).collect(),
            note: note.map(str::to_string),
        }
    }

    #[test]
    fn a_rewrite_changes_only_the_value_that_changed_and_keeps_the_rest_as_written() {
        let original = "{\n    \"list\": [\"a\"],\n    \"_mine\": [1,2],\n    \"version\": 1\n}\n";
        assert_eq!(
            rewrite(original, &shaped(&["a", "b"], None)),
            "{\n    \"list\": [\n        \"a\",\n        \"b\"\n    ],\n    \"_mine\": [1,2],\n    \
             \"version\": 1\n}\n"
        );
    }

    #[test]
    fn a_rewrite_drops_a_key_the_change_removed_and_puts_a_new_one_last() {
        let original = r#"{"version": 1, "list": ["a"]}"#;
        assert_eq!(
            rewrite(original, &shaped(&[], Some("n"))),
            "{\n  \"version\": 1,\n  \"note\": \"n\"\n}\n"
        );
    }

    #[test]
    fn a_rewrite_over_text_that_is_not_the_document_writes_it_whole() {
        let now = shaped(&["a"], None);
        assert_eq!(rewrite("not json", &now), render(&now));
        assert_eq!(rewrite("[1]", &now), render(&now));
    }

    #[test]
    fn a_written_document_reads_back_from_its_project_root() {
        let dir = crate::testdir::make("document-roundtrip");
        let probe = Probe {
            version: 2,
            held: "kept".to_string(),
        };
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        std::fs::write(dir.join(Probe::FILE), render(&probe)).unwrap();
        assert_eq!(read::<Probe>(&dir), Ok(Some(probe)));
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_written_where_the_process_runs() {
        let stray = Path::new("chock-document-refused-probe.json");
        let refused = write(stray, "{}\n").unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            refused.to_string().contains("not under a project root"),
            "{refused}"
        );
        assert!(
            !stray.exists(),
            "a relative write reached the working directory"
        );

        let dir = crate::testdir::make("document-rooted");
        write(&dir.join("kept.json"), "{}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("kept.json")).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn a_rendered_document_ends_in_a_newline_so_a_diff_reads_cleanly() {
        let text = render(&Probe {
            version: 2,
            held: "x".to_string(),
        });
        assert!(text.ends_with("}\n"), "{text}");
    }

    #[test]
    fn every_schema_a_document_names_is_published_in_this_repository() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for document in ["config", "baseline", "run"] {
            let url = schema_url(document, 1);
            let name = url.rsplit('/').next().unwrap();
            assert_eq!(name, format!("{document}-v1.json"));
            let path = repo.join("schema").join(name);
            assert!(
                path.is_file(),
                "{} is named but not published",
                path.display()
            );
        }
    }

    /// The schemas set `additionalProperties: false`, so an undeclared field rejects chock's file.
    #[test]
    fn every_field_a_document_writes_is_declared_in_its_published_schema() {
        let config = crate::project::config::Config {
            left_off: std::collections::BTreeMap::from([(
                "duplication".to_string(),
                "init found 4 findings".to_string(),
            )]),
            target: Some("wasm32-wasip1".to_string()),
            profile: Some("dist".to_string()),
            not_shipped: Some(vec!["benchmark/**".to_string()]),
            miri: Some(crate::project::config::Scope {
                packages: Some(vec!["outpost-core".to_string()]),
                exclude: Some(vec!["outpost-tabular".to_string()]),
                flags: Some(vec!["-Zmiri-tree-borrows".to_string()]),
            }),
            message: Some(crate::project::config::Message {
                subject: Some(72),
                body: Some(12),
            }),
            runner: Some(vec!["just".to_string(), "test".to_string()]),
            stage: [("binsize".to_string(), crate::project::config::Stage::Ci)].into(),
            commands: Some(vec![crate::project::config::Command {
                name: "doc-check".to_string(),
                run: vec!["just".to_string(), "doc-check".to_string()],
                counts: true,
                builds: true,
            }]),
            forbidden: Some(vec![crate::project::config::Forbidden {
                text: "mercurial".to_string(),
                why: "describe the behaviour instead".to_string(),
                word: true,
                except: vec!["**/vcs/**".to_string()],
            }]),
            history: Some(crate::project::config::History {
                accepted: vec![crate::project::config::Accepted {
                    commit: "a389111".to_string(),
                    path: "tests/fixtures/assertion.jwt".to_string(),
                    rule: "jwt".to_string(),
                    reason: "signed by the throwaway key beside it".to_string(),
                }],
            }),
            ..crate::project::config::Config::of(["slop"])
                .with_coverage(["just", "coverage", "{lcov}"])
        };
        let mut gate = crate::run::report::GateReport::new(
            "slop",
            crate::run::report::Verdict::Tripped,
            "chock run slop",
        );
        gate.measured = Some(3);
        gate.baseline = Some(2);
        gate.unit = Some("comment(s)".to_string());
        gate.cannot_run_reason = Some("clippy is not installed".to_string());
        gate.ran_at = Some(1_758_600_000);
        gate.findings = vec![
            crate::run::report::Finding::at("src/lib.rs", "a three-line comment block")
                .line(9)
                .item("f")
                .numbers(3, Some(2)),
        ];
        let baseline = {
            let mut recorded = crate::run::baseline::Baseline::empty("0.1.0");
            let mut series = crate::run::baseline::Series::new();
            series.set("src/lib.rs#f", 2);
            recorded.set("slop", series);
            recorded
        };
        let run = crate::run::report::Run::new("0.1.0", vec![gate]);
        for (document, written) in [
            ("config", serde_json::to_value(&config).unwrap()),
            ("baseline", serde_json::to_value(&baseline).unwrap()),
            ("run", serde_json::to_value(&run).unwrap()),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("schema")
                .join(format!("{document}-v1.json"));
            let schema: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let mut missing = Vec::new();
            undeclared(&schema, &written, "", &schema, &mut missing);
            assert_eq!(missing, Vec::<String>::new(), "{document}-v1.json");
        }
    }

    /// Collects each key in `written` its schema does not describe. Keys only, not a validator.
    fn undeclared(
        schema: &serde_json::Value,
        written: &serde_json::Value,
        at: &str,
        root: &serde_json::Value,
        missing: &mut Vec<String>,
    ) {
        if let serde_json::Value::Object(fields) = written {
            for (key, held) in fields {
                one_key(schema, key, held, at, root, missing);
            }
            return;
        }
        let (Some(items), Some(of)) = (written.as_array(), schema.get("items")) else {
            return;
        };
        let of = resolved(of, root);
        for held in items {
            undeclared(&of, held, &format!("{at}/[]"), root, missing);
        }
    }

    /// Checks one key against its subschema: `properties` by name, else `additionalProperties`.
    fn one_key(
        schema: &serde_json::Value,
        key: &str,
        held: &serde_json::Value,
        at: &str,
        root: &serde_json::Value,
        missing: &mut Vec<String>,
    ) {
        let named = schema.pointer(&format!("/properties/{key}"));
        let any = schema
            .get("additionalProperties")
            .filter(|of| of.is_object());
        let path = format!("{at}/{key}");
        match named.or(any) {
            Some(of) => undeclared(&resolved(of, root), held, &path, root, missing),
            None => missing.push(path),
        }
    }

    /// A `$ref` into the same file's `$defs`, which is how a schema names a repeated shape.
    fn resolved(schema: &serde_json::Value, root: &serde_json::Value) -> serde_json::Value {
        let Some(to) = schema.get("$ref").and_then(serde_json::Value::as_str) else {
            return schema.clone();
        };
        root.pointer(to.trim_start_matches('#'))
            .cloned()
            .unwrap_or_else(|| panic!("{to} names nothing"))
    }
}
