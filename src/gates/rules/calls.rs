//! Calls that go together, listed under `calls`: one that must come `after` another in the same
//! function, or one a function must pair `with` another, such as a lock and its release.

mod broken;

use broken::broken;

use crate::gates::metrics::prodlines::for_each_source;
use crate::gates::rules::Pattern;
use crate::project::config::Paired;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

/// The `calls` gate: calls that the project lists as a pair, out of order or alone.
pub const GATE: Gate = Gate {
    name: "calls",
    about: "a function makes a call listed under `calls` without the call it must come after or with",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "unpaired call(s)",
    },
};

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let rules = rules(&ctx.calls)?;
    let mut debt = Vec::new();
    for (shown, broken) in for_each_source(ctx, &|src| broken(src, &rules))? {
        for (index, function, line) in broken {
            debt.push(finding(&shown, &ctx.calls[index], &function, line));
        }
    }
    Ok(Inspection::debt(debt))
}

/// One entry as patterns: the call, and what must come before it or with it.
#[derive(Debug, PartialEq, Eq)]
struct Rule {
    call: Pattern,
    after: Option<Pattern>,
    with: Option<Pattern>,
}

/// Each entry as a rule. An entry with no call, nothing to pair it with, or no reason is refused.
fn rules(calls: &[Paired]) -> Result<Vec<Rule>, String> {
    if calls.is_empty() {
        return Err(format!(
            "no call is listed under `calls` in {}",
            crate::project::config::FILE
        ));
    }
    calls
        .iter()
        .map(|entry| {
            let named = &entry.call;
            let call = Pattern::of(named)
                .ok_or_else(|| format!("the `calls` entry `{named}` names no call"))?;
            let pattern = |other: Option<&String>, key: &str| match other {
                None => Ok(None),
                Some(other) => Pattern::of(other)
                    .map(Some)
                    .ok_or_else(|| format!("the `calls` entry `{named}` names no call in `{key}`")),
            };
            let after = pattern(entry.after.as_ref(), "after")?;
            let with = pattern(entry.with.as_ref(), "with")?;
            if after.is_none() && with.is_none() {
                return Err(format!(
                    "the `calls` entry `{named}` names no `after` and no `with`"
                ));
            }
            if entry.why.trim().is_empty() {
                return Err(format!("the `calls` entry `{named}` gives no `why`"));
            }
            Ok(Rule { call, after, with })
        })
        .collect()
}

fn finding(shown: &str, entry: &Paired, function: &str, line: u32) -> Finding {
    let call = &entry.call;
    let needs = [
        entry.after.as_ref().map(|after| format!("after `{after}`")),
        entry.with.as_ref().map(|with| format!("with `{with}`")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" and ");
    let told = format!(
        "`{function}` calls `{call}`, which must come {needs}: {}",
        entry.why
    );
    Finding::at(shown, &told)
        .item(&format!("{call} in {function}"))
        .line(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(call: &str, after: Option<&str>, with: Option<&str>) -> Paired {
        Paired {
            call: call.to_string(),
            after: after.map(str::to_string),
            with: with.map(str::to_string),
            why: "they go together".to_string(),
        }
    }

    fn listed() -> [Paired; 2] {
        [
            entry("delete", Some("authorize"), None),
            entry("reserve_name", None, Some("release_name")),
        ]
    }

    const SOURCE: &str = r#"use store::delete as remove;
fn handler(user: User) {
    authorize(&user);
    store::delete(user.id);
}
fn unchecked(id: Id) {
    remove(id);
    authorize(&id);
}
fn nested(id: Id) {
    store::delete(authorize(id));
}
fn closure(id: Id) {
    let run = || store::delete(id);
    run();
}
impl Names {
    fn take(&self) { self.reserve_name("a"); }
    fn both(&self) { self.reserve_name("a"); self.release_name("a"); }
    fn inner(&self) { fn helper() { reserve_name("a") } release_name("b"); }
}
trait Hold { fn grab(&self) { self.reserve_name("x") } fn none(&self); }
#[cfg(test)]
mod tests { fn t() { delete(1); } }
#[test]
fn t() { store::delete(1); }
impl Tr for &Names { fn f(&self) { names::reserve_name(1) } }
static S: X = reserve_name(2);
impl Names { #[test] fn checked(&self) { self.reserve_name("t"); } }
#[cfg(test)]
trait Faked { fn grab(&self) { self.reserve_name("t") } }
"#;

    #[test]
    fn a_function_breaks_a_rule_when_the_call_comes_first_or_alone() {
        let rules = rules(&listed()).unwrap();
        let found = broken(SOURCE, &rules).unwrap();
        let named: Vec<(usize, &str, u32)> = found
            .iter()
            .map(|(index, function, line)| (*index, function.as_str(), *line))
            .collect();
        assert_eq!(
            named,
            [
                (0, "unchecked", 7),
                (0, "closure", 14),
                (1, "Names::take", 18),
                (1, "helper", 20),
                (1, "Hold::grab", 22),
                (1, "impl::f", 27),
            ]
        );
        assert!(broken("fn broken( {", &rules).is_err());
    }

    #[test]
    fn a_rule_with_both_needs_the_earlier_call_and_the_partner() {
        let rules = rules(&[entry(".open", Some("check"), Some(".close"))]).unwrap();
        let broken = |body: &str| {
            broken(&format!("fn f() {{ {body} }}"), &rules)
                .unwrap()
                .len()
        };
        assert_eq!(broken("check(); x.open(); x.close();"), 0);
        assert_eq!(broken("x.open(); check(); x.close();"), 1);
        assert_eq!(broken("check(); x.open();"), 1);
        assert_eq!(
            broken("check(); open(); close();"),
            0,
            "a path call is no method call"
        );
        assert_eq!(broken("x.open(check());"), 1, "no partner");
        assert_eq!(broken("x.open(check()); x.close()"), 0);
    }

    #[test]
    fn an_entry_names_a_call_what_it_pairs_with_and_a_reason() {
        let refused = |entry: Paired| rules(&[entry]).unwrap_err();
        assert_eq!(
            rules(&[]).unwrap_err(),
            "no call is listed under `calls` in .chock/config.json"
        );
        assert_eq!(
            refused(entry("", Some("a"), None)),
            "the `calls` entry `` names no call"
        );
        assert_eq!(
            refused(entry("delete", Some("::"), None)),
            "the `calls` entry `delete` names no call in `after`"
        );
        assert_eq!(
            refused(entry("delete", None, Some(""))),
            "the `calls` entry `delete` names no call in `with`"
        );
        assert_eq!(
            refused(entry("delete", None, None)),
            "the `calls` entry `delete` names no `after` and no `with`"
        );
        let unexplained = Paired {
            why: " ".to_string(),
            ..entry("delete", Some("a"), None)
        };
        assert_eq!(
            refused(unexplained),
            "the `calls` entry `delete` gives no `why`"
        );
    }

    #[test]
    fn a_finding_names_the_function_and_what_the_call_needs() {
        let render = |entry: &Paired| finding("src/a.rs", entry, "f", 4).render();
        assert_eq!(
            render(&entry("delete", Some("authorize"), None)),
            "src/a.rs:4: delete in f: `f` calls `delete`, which must come after `authorize`: \
             they go together"
        );
        assert_eq!(
            render(&entry("lock", None, Some("unlock"))),
            "src/a.rs:4: lock in f: `f` calls `lock`, which must come with `unlock`: they go \
             together"
        );
        assert_eq!(
            render(&entry("open", Some("check"), Some("close"))),
            "src/a.rs:4: open in f: `f` calls `open`, which must come after `check` and with \
             `close`: they go together"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_reads_every_production_file() {
        let mut held = crate::testdir::Held::tree(
            "calls-tree",
            &[
                ("src/lib.rs", SOURCE),
                ("tests/end.rs", "fn t() { delete(1); }\n"),
            ],
        );
        held.calls = listed().to_vec();
        let debt = inspect(&held).unwrap().debt;
        assert_eq!(
            debt.first().map(Finding::render).unwrap(),
            "src/lib.rs:7: delete in unchecked: `unchecked` calls `delete`, which must come after \
             `authorize`: they go together"
        );
        let places: Vec<(&str, Option<u32>)> =
            debt.iter().map(|f| (f.file.as_str(), f.line)).collect();
        let lines = [7, 14, 18, 20, 22, 27].map(|line| ("src/lib.rs", Some(line)));
        assert_eq!(places, lines);
        held.calls = Vec::new();
        assert!(inspect(&held).is_err());
    }
}
