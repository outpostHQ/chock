//! Values one file should own, listed under `owners` by the call that reads them: a setting read
//! through `std::env::var` in two files drifts apart when one of them changes.

use std::collections::{BTreeMap, BTreeSet};

use syn::visit::Visit;

use crate::gates::metrics::prodlines::{for_each_source, parse_rust};
use crate::gates::rules::{Aliases, Callee, Pattern, line_of, production};
use crate::project::config::Owner;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "owners",
    about: "a value read through a call listed under `owners` is read in more than one file",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "read(s) of a value another file reads",
    },
};

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let patterns = patterns(&ctx.owners)?;
    let read = for_each_source(ctx, &|src| reads(src, &patterns))?;
    Ok(Inspection::debt(shared(&read, &ctx.owners)))
}

/// Each listed call as a pattern. An entry that names no call or gives no reason is refused.
fn patterns(owners: &[Owner]) -> Result<Vec<Pattern>, String> {
    if owners.is_empty() {
        return Err(format!(
            "no call is listed under `owners` in {}",
            crate::project::config::FILE
        ));
    }
    owners
        .iter()
        .map(|owner| {
            let pattern = Pattern::of(&owner.call)
                .ok_or_else(|| format!("the `owners` entry `{}` names no call", owner.call))?;
            if owner.why.trim().is_empty() {
                return Err(format!("the `owners` call `{}` gives no `why`", owner.call));
            }
            Ok(pattern)
        })
        .collect()
}

/// One read: the listed call by its place in `owners`, the value it reads, and the line.
type Read = (usize, String, u32);

/// Each read of a value through a listed call in production code. A value chock cannot name, such
/// as a variable or a `format!`, is not a read.
fn reads(src: &str, patterns: &[Pattern]) -> Result<Vec<Read>, String> {
    let file = parse_rust(src)?;
    let mut found = Reads {
        aliases: Aliases::of(&file),
        patterns,
        seen: Vec::new(),
    };
    for item in production(&file.items) {
        found.visit_item(item);
    }
    Ok(found.seen)
}

struct Reads<'a> {
    aliases: Aliases,
    patterns: &'a [Pattern],
    seen: Vec<Read>,
}

impl Reads<'_> {
    fn read(&mut self, callee: &Callee, value: Option<String>, line: u32) {
        let Some(value) = value else {
            return;
        };
        for (index, pattern) in self.patterns.iter().enumerate() {
            if pattern.calls(callee) {
                self.seen.push((index, value.clone(), line));
            }
        }
    }
}

impl<'ast> Visit<'ast> for Reads<'_> {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Some(callee) = Callee::of(node, &self.aliases) {
            let line = line_of(node.paren_token.span.open());
            self.read(&callee, node.args.first().and_then(value), line);
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let callee = Callee::Method(node.method.to_string());
        let line = line_of(node.method.span());
        self.read(&callee, node.args.first().and_then(value), line);
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// The value an argument names: a string literal, shown in quotes, or a constant by its last
/// segment. A borrow or parentheses around it are looked through.
fn value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Reference(inner) => value(&inner.expr),
        syn::Expr::Paren(inner) => value(&inner.expr),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => Some(format!("{:?}", text.value())),
        syn::Expr::Path(path) if path.qself.is_none() => {
            let last = path.path.segments.last()?.ident.to_string();
            let constant = last.chars().any(|c| c.is_ascii_uppercase())
                && last
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            constant.then_some(last)
        }
        _ => None,
    }
}

/// A finding for each read of a value that two or more files read through the same listed call.
fn shared(read: &[(String, Vec<Read>)], owners: &[Owner]) -> Vec<Finding> {
    let mut readers: BTreeMap<(usize, &str), BTreeSet<&str>> = BTreeMap::new();
    for (shown, reads) in read {
        for (index, value, _) in reads {
            readers
                .entry((*index, value.as_str()))
                .or_default()
                .insert(shown.as_str());
        }
    }
    let mut found = Vec::new();
    for (shown, reads) in read {
        for (index, value, line) in reads {
            let files = &readers[&(*index, value.as_str())];
            if files.len() < 2 {
                continue;
            }
            let owner = &owners[*index];
            let item = format!("{}({value})", owner.call);
            let told = format!(
                "`{item}` is read in {} files ({}): {}",
                files.len(),
                listed(files),
                owner.why
            );
            found.push(Finding::at(shown, &told).item(&item).line(*line));
        }
    }
    found
}

/// The first three files, then how many more.
fn listed(files: &BTreeSet<&str>) -> String {
    let named: Vec<&str> = files.iter().take(3).copied().collect();
    match files.len().saturating_sub(3) {
        0 => named.join(", "),
        more => format!("{} and {more} more", named.join(", ")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn owner(call: &str) -> Owner {
        Owner {
            call: call.to_string(),
            why: "one module owns each setting".to_string(),
        }
    }

    fn read(src: &str, calls: &[&str]) -> Vec<(usize, String, u32)> {
        let patterns: Vec<Pattern> = calls
            .iter()
            .map(|call| Pattern::of(call).unwrap())
            .collect();
        reads(src, &patterns).unwrap()
    }

    #[test]
    fn a_read_names_a_literal_or_a_constant_through_the_file_s_aliases() {
        let src = "use std::env;\nuse std::env::var as get;\nconst HOME: &str = \"HOME\";\n\
                   fn f(name: &str) {\n    env::var(\"PATH\");\n    get(&(HOME));\n\
                   std::env::var(name);\n    env::var(config::KEY_2);\n    env::var(Key); env::var(<T as K>::KEY_3);\n\
                   cfg.lookup(\"PATH\");\n    std::env::var_os(\"PATH\");\n    env::var(x_Y); env::var(1);\n}\n\
                   #[cfg(test)]\nmod tests { fn t() { std::env::var(\"PATH\"); } }\n\
                   #[test]\nfn t() { std::env::var(\"PATH\"); }\n";
        let quoted = |text: &str| format!("{text:?}");
        assert_eq!(
            read(src, &["std::env::var", ".lookup"]),
            [
                (0, quoted("PATH"), 5),
                (0, "HOME".to_string(), 6),
                (0, "KEY_2".to_string(), 8),
                (1, quoted("PATH"), 10),
            ]
        );
        assert_eq!(read(src, &["lookup"]), [(0, quoted("PATH"), 10)]);
        assert!(reads("fn broken( {", &[]).is_err());
    }

    #[test]
    fn each_read_of_a_value_two_files_read_is_found_and_names_the_files() {
        let owners = [owner("std::env::var"), owner(".lookup")];
        let path = || "\"PATH\"".to_string();
        let read = |shown: &str, reads: Vec<Read>| (shown.to_string(), reads);
        let files = [
            read(
                "src/a.rs",
                vec![(0, path(), 3), (1, path(), 4), (0, path(), 9)],
            ),
            read("src/b.rs", vec![(0, path(), 1), (0, "HOME".into(), 2)]),
            read("src/c.rs", vec![(0, path(), 7)]),
            read("src/d.rs", vec![(0, path(), 2)]),
            read("src/e.rs", vec![(0, "HOME".into(), 5)]),
        ];
        let rendered: Vec<String> = shared(&files, &owners)
            .iter()
            .map(Finding::render)
            .collect();
        let path_told = "std::env::var(\"PATH\"): `std::env::var(\"PATH\")` is read in 4 files \
                         (src/a.rs, src/b.rs, src/c.rs and 1 more): one module owns each setting";
        let home_told = "std::env::var(HOME): `std::env::var(HOME)` is read in 2 files \
                         (src/b.rs, src/e.rs): one module owns each setting";
        assert_eq!(
            rendered,
            [
                format!("src/a.rs:3: {path_told}"),
                format!("src/a.rs:9: {path_told}"),
                format!("src/b.rs:1: {path_told}"),
                format!("src/b.rs:2: {home_told}"),
                format!("src/c.rs:7: {path_told}"),
                format!("src/d.rs:2: {path_told}"),
                format!("src/e.rs:5: {home_told}"),
            ]
        );
    }

    #[test]
    fn an_entry_names_a_call_and_gives_a_reason() {
        assert_eq!(
            patterns(&[]).unwrap_err(),
            "no call is listed under `owners` in .chock/config.json"
        );
        assert_eq!(
            patterns(&[owner("::")]).unwrap_err(),
            "the `owners` entry `::` names no call"
        );
        let unexplained = Owner {
            why: String::new(),
            ..owner("env::var")
        };
        assert_eq!(
            patterns(&[unexplained]).unwrap_err(),
            "the `owners` call `env::var` gives no `why`"
        );
        assert_eq!(
            patterns(&[owner("env::var")]).unwrap(),
            [Pattern::Path(vec!["env".to_string(), "var".to_string()])]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_reads_every_production_file() {
        let mut held = crate::testdir::Held::tree(
            "owners-tree",
            &[
                (
                    "src/lib.rs",
                    "mod b;\nfn a() { std::env::var(\"PORT\"); }\n",
                ),
                (
                    "src/b.rs",
                    "fn b() { std::env::var(\"PORT\"); std::env::var(\"HOST\"); }\n",
                ),
                ("tests/end.rs", "fn t() { std::env::var(\"HOST\"); }\n"),
            ],
        );
        held.owners = vec![owner("std::env::var")];
        let debt = inspect(&held).unwrap().debt;
        let mut lines: Vec<String> = debt
            .iter()
            .map(|found| format!("{}:{:?}", found.file, found.line))
            .collect();
        lines.sort();
        assert_eq!(lines, ["src/b.rs:Some(1)", "src/lib.rs:Some(2)"]);
        held.owners = Vec::new();
        assert!(inspect(&held).is_err());
    }
}
