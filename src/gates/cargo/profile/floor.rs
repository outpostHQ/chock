//! The lint floor: lints each effective lint table holds at `warn` or above, so a crate keeps out
//! panics, stray output, unchecked casts and undocumented items from its first commit.

use std::collections::BTreeMap;
use std::path::Path;

use super::{CLIPPY, MANIFEST, RUST};
use super::{Doc, Review, Sources, Tool, Value, advisory, levels, parse, slurp};
use crate::project::release::{LINT_REASONS, Release};
use crate::run::report::Finding;

/// How `lint_floor` holds the floor: absent advises, `true` fails the gate, `false` skips it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Floor {
    /// No `lint_floor` key: each gap is an advisory and the gate still passes.
    #[default]
    Advised,
    /// `"lint_floor": true`: each gap fails the gate.
    Enforced,
    /// `"lint_floor": false`: the floor is not checked.
    Off,
}

impl Floor {
    /// The mode the `lint_floor` key asks for.
    #[must_use]
    pub fn of(setting: Option<bool>) -> Self {
        match setting {
            None => Self::Advised,
            Some(true) => Self::Enforced,
            Some(false) => Self::Off,
        }
    }
}

/// The rustc lints and the one rustc group the floor names.
const RUSTC_LINTS: [&str; 5] = [
    "missing_docs",
    "missing_debug_implementations",
    "unreachable_pub",
    "unsafe_op_in_unsafe_fn",
    "rust_2018_idioms",
];

/// Two clippy groups, then lints outside every group, which only a lint table turns on.
const CLIPPY_LINTS: [&str; 12] = [
    "all",
    "pedantic",
    "unwrap_used",
    "allow_attributes",
    "allow_attributes_without_reason",
    "undocumented_unsafe_blocks",
    "multiple_unsafe_ops_per_block",
    "clone_on_ref_ptr",
    "dbg_macro",
    "todo",
    "print_stdout",
    "print_stderr",
];

/// Pedantic lints the floor names. The `pedantic` group meets each one, unless the table lowers
/// that one lint itself.
const PEDANTIC_LINTS: [&str; 6] = [
    "missing_panics_doc",
    "missing_errors_doc",
    "fn_params_excessive_bools",
    "struct_excessive_bools",
    "cast_possible_truncation",
    "cast_sign_loss",
];

/// The groups among the floor. Each takes a lower priority than the single lints it holds.
const GROUPS: [&str; 3] = ["rust_2018_idioms", "all", "pedantic"];

/// The levels that meet the floor; `allow`, or a level cargo does not know, does not.
const HELD: [&str; 3] = ["warn", "deny", "forbid"];

/// The lints clippy checks only in a crate whose `rust-version` builds `reason = "…"`.
const REASON_LINTS: [&str; 2] = ["allow_attributes", "allow_attributes_without_reason"];

/// How many `bool` parameters clippy lets a function take before `fn_params_excessive_bools`.
const PARAMS_BOOLS: &str = "max-fn-params-bools";

/// One effective lint table: `lints` in a member, or the root's `workspace.lints` that its members
/// inherit.
pub(super) struct Table<'a> {
    pub(super) doc: &'a Doc,
    pub(super) key: &'a str,
    pub(super) file: &'a str,
}

/// Holds one lint table to the floor. `promise` is the lowest `rust-version` among the crates that
/// read the table.
pub(super) fn hold(table: &Table<'_>, floor: Floor, promise: Option<Release>, review: &mut Review) {
    let tools = [
        (RUST, &RUSTC_LINTS[..], &[][..]),
        (CLIPPY, &CLIPPY_LINTS[..], &PEDANTIC_LINTS[..]),
    ];
    for (tool, named, pedantic) in tools {
        let set = set_levels(table.doc, table.key, tool.name);
        let line = header(table.doc, table.key, tool.name);
        for (name, level) in gaps(&set, named, pedantic) {
            if repeats_a_failure(&tool, name, level) {
                continue;
            }
            let advice = advice(&format!("{}.{}", table.key, tool.name), name, level);
            let message = format!("{advice}{}", note(name, promise));
            let item = format!("{}::{name}", tool.name);
            record(
                review,
                floor,
                Finding::at(table.file, &message).line(line).item(&item),
            );
        }
    }
}

/// The level the table sets for each lint of one tool, keyed by its name with `_` for `-`.
fn set_levels(doc: &Doc, table: &str, tool: &str) -> BTreeMap<String, String> {
    levels(doc, table, tool)
        .into_iter()
        .map(|(rule, level, _)| (rule.replace('-', "_"), level))
        .collect()
}

/// The floor lints the table leaves below `warn`, each with the level it sets, if any. A named
/// pedantic lint is a gap only where the table lowers it, as the `pedantic` group holds the rest.
fn gaps<'a>(
    set: &'a BTreeMap<String, String>,
    named: &[&'static str],
    pedantic: &[&'static str],
) -> Vec<(&'static str, Option<&'a str>)> {
    let below = |name: &&'static str| {
        let level = set.get(*name).map(String::as_str);
        (!level.is_some_and(|level| HELD.contains(&level))).then_some((*name, level))
    };
    let lowered = |gap: &(&'static str, Option<&'a str>)| gap.1.is_some();
    named
        .iter()
        .filter_map(below)
        .chain(pedantic.iter().filter_map(below).filter(lowered))
        .collect()
}

/// Whether `permissive` already fails this gap, an `allow` on a lint it catalogues, so one fault
/// is named once.
fn repeats_a_failure(tool: &Tool, name: &str, level: Option<&str>) -> bool {
    level == Some("allow") && tool.cost(name).is_some()
}

/// The line of the table a gap belongs under: the tool's own, else the lint table, else 1.
fn header(doc: &Doc, table: &str, tool: &str) -> u32 {
    doc.tables
        .get(&format!("{table}.{tool}"))
        .or_else(|| doc.tables.get(table))
        .copied()
        .unwrap_or(1)
}

/// What to write so the lint meets the floor. A group gets priority -1, so a single lint the
/// table names still overrides it.
fn advice(table: &str, name: &str, level: Option<&str>) -> String {
    let wanted = if GROUPS.contains(&name) {
        "{ level = \"deny\", priority = -1 }"
    } else {
        "\"deny\""
    };
    match level {
        None => format!("add {name} = {wanted} under [{table}]"),
        Some(level) => format!("set {name} = {wanted} under [{table}], in place of \"{level}\""),
    }
}

/// What a reader needs to know beyond the line to write: that clippy skips a reason lint below
/// the Rust release it needs, and that tests may keep `unwrap`.
fn note(name: &str, promise: Option<Release>) -> String {
    match promise {
        Some(promise) if REASON_LINTS.contains(&name) && !LINT_REASONS.open_to(Some(promise)) => {
            format!(
                " (clippy checks it from rust-version {} on; rust-version here is {promise})",
                LINT_REASONS.since
            )
        }
        _ if name == "unwrap_used" => {
            " (`allow-unwrap-in-tests = true` in clippy.toml keeps it out of tests)".to_string()
        }
        _ => String::new(),
    }
}

/// Records a gap: a failure where `lint_floor` is `true`, an advisory beside the others where it
/// is absent, and nothing where it is `false`.
fn record(review: &mut Review, floor: Floor, finding: Finding) {
    match floor {
        Floor::Enforced => review.fail(finding),
        Floor::Advised => review.floor.push(advisory(finding)),
        Floor::Off => {}
    }
}

/// clippy's configuration at the root, which clippy reads as `.clippy.toml` before `clippy.toml`.
pub(super) fn clippy_file(root: &Path) -> Result<Option<(String, String)>, String> {
    for name in [".clippy.toml", "clippy.toml"] {
        let path = root.join(name);
        if path.is_file() {
            return Ok(Some((name.to_string(), slurp(&path)?)));
        }
    }
    Ok(None)
}

/// Holds clippy's configuration to `max-fn-params-bools = 0`, so `fn_params_excessive_bools`
/// names every `bool` parameter, whose meaning a call site cannot show.
pub(super) fn clippy_config(sources: &Sources, review: &mut Review) -> Result<(), String> {
    if sources.floor == Floor::Off {
        return Ok(());
    }
    let (file, line, says) = match &sources.clippy {
        None => (
            MANIFEST,
            1,
            format!("add a clippy.toml beside this manifest that sets {PARAMS_BOOLS} = 0"),
        ),
        Some((file, text)) => {
            let doc = parse(text).map_err(|e| format!("{file}: {e}"))?;
            match (doc.get(PARAMS_BOOLS), doc.line(PARAMS_BOOLS)) {
                (Some(Value::Int(0)), _) => return Ok(()),
                (Some(_), Some(line)) => (
                    file.as_str(),
                    line,
                    format!("set {PARAMS_BOOLS} = 0 in place of this value"),
                ),
                _ => (file.as_str(), 1, format!("add {PARAMS_BOOLS} = 0")),
            }
        }
    };
    let gap = Finding::at(file, &says).line(line).item(PARAMS_BOOLS);
    record(review, sources.floor, gap);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::super::review;
    use super::*;

    /// A lint table that meets each entry of the floor, written out in full: `[lints.clippy]` is
    /// line 7.
    const FLOOR: &str = "[lints.rust]\n\
        missing_docs = \"deny\"\n\
        missing_debug_implementations = \"deny\"\n\
        unreachable_pub = \"deny\"\n\
        unsafe_op_in_unsafe_fn = \"deny\"\n\
        rust_2018_idioms = { level = \"deny\", priority = -1 }\n\
        [lints.clippy]\n\
        all = { level = \"deny\", priority = -1 }\n\
        pedantic = { level = \"warn\", priority = -1 }\n\
        unwrap_used = \"deny\"\n\
        allow_attributes = \"deny\"\n\
        allow_attributes_without_reason = \"deny\"\n\
        undocumented_unsafe_blocks = \"deny\"\n\
        multiple_unsafe_ops_per_block = \"deny\"\n\
        clone_on_ref_ptr = \"deny\"\n\
        dbg_macro = \"deny\"\n\
        todo = \"deny\"\n\
        print_stdout = \"deny\"\n\
        print_stderr = \"deny\"\n";

    /// The item of each gap in a lint table that sets nothing, in the order the gate names them.
    const EVERY_LINT: &str = "rust::missing_docs rust::missing_debug_implementations \
        rust::unreachable_pub rust::unsafe_op_in_unsafe_fn rust::rust_2018_idioms clippy::all \
        clippy::pedantic clippy::unwrap_used clippy::allow_attributes \
        clippy::allow_attributes_without_reason clippy::undocumented_unsafe_blocks \
        clippy::multiple_unsafe_ops_per_block clippy::clone_on_ref_ptr clippy::dbg_macro \
        clippy::todo clippy::print_stdout clippy::print_stderr";

    /// One crate whose manifest is `manifest`, with a clippy config at zero, held as `floor` says.
    fn crate_of(manifest: &str, floor: Floor) -> Sources {
        let mut sources = Sources {
            root: manifest.to_string(),
            clippy: Some((
                "clippy.toml".to_string(),
                "max-fn-params-bools = 0\n".to_string(),
            )),
            floor,
            ..Sources::default()
        };
        sources
            .members
            .insert("Cargo.toml".to_string(), manifest.to_string());
        sources
    }

    /// The item of each failure and floor advisory, without the `advisory, ` label.
    fn gaps_in(sources: &Sources) -> Vec<String> {
        let read = review(sources).unwrap();
        read.failures
            .iter()
            .chain(&read.floor)
            .filter_map(|finding| finding.item.clone())
            .map(|item| item.trim_start_matches("advisory, ").to_string())
            .collect()
    }

    fn failures(sources: &Sources) -> Vec<String> {
        review(sources)
            .unwrap()
            .failures
            .iter()
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn a_table_holding_every_floor_lint_with_no_bool_parameter_allowed_has_no_gap() {
        for floor in [Floor::Advised, Floor::Enforced] {
            assert_eq!(gaps_in(&crate_of(FLOOR, floor)), Vec::<String>::new());
        }
    }

    #[test]
    fn a_crate_with_no_lint_table_is_named_once_for_each_floor_lint_at_line_one() {
        let read = review(&crate_of("[package]\nname = \"x\"\n", Floor::Enforced)).unwrap();
        // A gap at another line drops out, and the list no longer matches.
        let at_one = |gap: &Finding| gap.item.clone().filter(|_| gap.line == Some(1));
        let items: Vec<String> = read.failures.iter().filter_map(at_one).collect();
        assert_eq!(items.join(" "), EVERY_LINT);
    }

    #[test]
    fn allow_falls_below_the_floor_where_warn_deny_and_forbid_meet_it() {
        let set = |level: &str| {
            FLOOR.replace(
                "missing_docs = \"deny\"",
                &format!("missing_docs = \"{level}\""),
            )
        };
        assert_eq!(
            gaps_in(&crate_of(&set("allow"), Floor::Enforced)),
            ["rust::missing_docs"]
        );
        for level in ["warn", "deny", "forbid"] {
            let met = gaps_in(&crate_of(&set(level), Floor::Enforced));
            assert_eq!(met, Vec::<String>::new(), "{level}");
        }
    }

    #[test]
    fn a_level_in_the_detailed_form_or_spelled_with_hyphens_is_read_like_a_plain_one() {
        let detailed = FLOOR.replace(
            "missing_debug_implementations = \"deny\"",
            "missing-debug-implementations = { level = \"warn\", priority = 1 }",
        );
        assert_eq!(
            gaps_in(&crate_of(&detailed, Floor::Enforced)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_pedantic_group_holds_its_named_lints_unless_the_table_allows_one() {
        let allowed = format!("{FLOOR}missing_panics_doc = \"allow\"\ncast_sign_loss = \"warn\"\n");
        assert_eq!(
            gaps_in(&crate_of(&allowed, Floor::Enforced)),
            ["clippy::missing_panics_doc"]
        );
        let no_group = FLOOR.replace("pedantic = { level = \"warn\", priority = -1 }\n", "");
        assert_eq!(
            gaps_in(&crate_of(&no_group, Floor::Enforced)),
            ["clippy::pedantic"]
        );
    }

    #[test]
    fn allowing_module_name_repetitions_or_must_use_candidate_is_no_gap() {
        let allowed =
            format!("{FLOOR}module_name_repetitions = \"allow\"\nmust-use-candidate = \"allow\"\n");
        assert_eq!(
            gaps_in(&crate_of(&allowed, Floor::Enforced)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_absent_key_advises_true_fails_and_false_leaves_the_floor_unchecked() {
        assert_eq!(
            [None, Some(true), Some(false)].map(Floor::of),
            [Floor::Advised, Floor::Enforced, Floor::Off]
        );
        let bare = Sources {
            clippy: None,
            ..crate_of("[package]\nname = \"x\"\n", Floor::Advised)
        };
        let held = |floor: Floor| {
            let read = review(&Sources {
                floor,
                ..bare.clone()
            })
            .unwrap();
            let items = |gaps: Vec<Finding>| gaps.into_iter().filter_map(|gap| gap.item);
            [read.failures, read.floor].map(|gaps| items(gaps).collect::<Vec<String>>())
        };
        let every: Vec<String> = EVERY_LINT
            .split(' ')
            .chain([PARAMS_BOOLS])
            .map(str::to_string)
            .collect();
        let advised = every
            .iter()
            .map(|item| format!("advisory, {item}"))
            .collect();
        assert_eq!(held(Floor::Advised), [Vec::new(), advised]);
        assert_eq!(held(Floor::Enforced), [every, Vec::new()]);
        assert_eq!(held(Floor::Off), [Vec::<String>::new(), Vec::new()]);
    }

    #[test]
    fn a_gap_points_at_its_table_header_and_says_what_to_write_there() {
        let said = |from: &str, to: &str| {
            failures(&crate_of(&FLOOR.replace(from, to), Floor::Enforced)).join("\n")
        };
        let unwrap = "Cargo.toml:7: clippy::unwrap_used: add unwrap_used = \"deny\" under \
                      [lints.clippy] (`allow-unwrap-in-tests = true` in clippy.toml keeps it out \
                      of tests)";
        assert_eq!(said("unwrap_used = \"deny\"\n", ""), unwrap);
        let all = "Cargo.toml:7: clippy::all: add all = { level = \"deny\", priority = -1 } under \
                   [lints.clippy]";
        assert_eq!(said("all = { level = \"deny\", priority = -1 }\n", ""), all);
        let todo = "Cargo.toml:7: clippy::todo: set todo = \"deny\" under [lints.clippy], in place \
                    of \"allow\"";
        assert_eq!(said("todo = \"deny\"", "todo = \"allow\""), todo);
    }

    #[test]
    fn an_allow_the_catalogue_already_fails_is_named_once_and_the_gaps_after_it_still_are() {
        let allowed = FLOOR
            .replace("unwrap_used = \"deny\"", "unwrap_used = \"allow\"")
            .replace("print_stderr = \"deny\"\n", "");
        let failed = review(&crate_of(&allowed, Floor::Enforced))
            .unwrap()
            .failures;
        let items: Vec<&str> = failed
            .iter()
            .filter_map(|finding| finding.item.as_deref())
            .collect();
        assert_eq!(items, ["clippy::unwrap_used", "clippy::print_stderr"]);
        assert!(
            failed[0].message.contains("allowed for the whole crate"),
            "{failed:?}"
        );
    }

    #[test]
    fn members_inheriting_the_workspace_lints_are_held_to_the_floor_once_at_the_root() {
        let inherited = FLOOR
            .replace("[lints.", "[workspace.lints.")
            .replace("todo = \"deny\"\n", "");
        let mut sources = crate_of(&format!("[workspace]\n{inherited}"), Floor::Enforced);
        sources.members.clear();
        for member in ["crates/one/Cargo.toml", "crates/two/Cargo.toml"] {
            sources.members.insert(
                member.to_string(),
                "[lints]\nworkspace = true\n".to_string(),
            );
        }
        assert_eq!(
            failures(&sources),
            ["Cargo.toml:8: clippy::todo: add todo = \"deny\" under [workspace.lints.clippy]"]
        );
    }

    #[test]
    fn below_rust_1_81_a_missing_reason_lint_says_clippy_skips_it_at_that_promise() {
        let without = FLOOR.replace("allow_attributes = \"deny\"\n", "");
        let promised = |release: &str| {
            let mut sources = crate_of(&without, Floor::Enforced);
            let promise = Release::parse(release).unwrap();
            sources.promises.insert(String::new(), promise);
            failures(&sources)
        };
        let gap = "Cargo.toml:7: clippy::allow_attributes: add allow_attributes = \"deny\" under \
                   [lints.clippy]";
        assert_eq!(
            promised("1.70"),
            [format!(
                "{gap} (clippy checks it from rust-version 1.81 on; rust-version here is 1.70)"
            )]
        );
        assert_eq!(promised("1.81"), [gap]);
        assert_eq!(failures(&crate_of(&without, Floor::Enforced)), [gap]);
    }

    #[test]
    fn the_workspace_table_takes_the_lowest_promise_among_the_members_that_inherit_it() {
        let inherited = FLOOR
            .replace("[lints.", "[workspace.lints.")
            .replace("allow_attributes_without_reason = \"deny\"\n", "");
        let mut sources = crate_of(&format!("[workspace]\n{inherited}"), Floor::Enforced);
        sources.members.clear();
        for (dir, promise) in [
            ("one", Some("1.80")),
            ("two", Some("1.60")),
            ("three", None),
        ] {
            let manifest = format!("crates/{dir}/Cargo.toml");
            let lints = "[lints]\nworkspace = true\n".to_string();
            sources.members.insert(manifest, lints);
            if let Some(promise) = promise.and_then(Release::parse) {
                sources.promises.insert(format!("crates/{dir}"), promise);
            }
        }
        sources
            .promises
            .insert("crates/own".to_string(), Release::new(1, 50));
        let found = failures(&sources);
        let lowest = |one: &String| one.ends_with("rust-version here is 1.60)");
        assert!(
            matches!(found.as_slice(), [one] if lowest(one)),
            "{found:?}"
        );
    }

    #[test]
    fn the_clippy_config_must_let_no_function_take_a_bool_parameter() {
        let with = |config: Option<&str>| {
            failures(&Sources {
                clippy: config.map(|text| ("clippy.toml".to_string(), text.to_string())),
                ..crate_of(FLOOR, Floor::Enforced)
            })
            .join("\n")
        };
        let set = "clippy.toml:2: max-fn-params-bools: set max-fn-params-bools = 0 in place of \
                   this value";
        let add = "clippy.toml:1: max-fn-params-bools: add max-fn-params-bools = 0";
        let written = [
            "max-fn-params-bools = 0\n",
            "msrv = \"1.80\"\nmax-fn-params-bools = 3\n",
            "msrv = \"1.80\"\n",
        ];
        assert_eq!(written.map(|text| with(Some(text))), ["", set, add]);
        let make = "Cargo.toml:1: max-fn-params-bools: add a clippy.toml beside this manifest that \
                    sets max-fn-params-bools = 0";
        assert_eq!(with(None), make);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_dotted_clippy_config_is_read_before_the_plain_one_as_clippy_reads_them() {
        let dir = crate::testdir::make("floor-clippy-config");
        assert_eq!(clippy_file(&dir), Ok(None));
        std::fs::write(dir.join("clippy.toml"), "max-fn-params-bools = 3\n").unwrap();
        std::fs::write(dir.join(".clippy.toml"), "max-fn-params-bools = 0\n").unwrap();
        assert_eq!(
            clippy_file(&dir),
            Ok(Some((
                ".clippy.toml".to_string(),
                "max-fn-params-bools = 0\n".to_string()
            )))
        );
    }
}
