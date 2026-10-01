//! `unsafe` outside test code, counted per file. A ratchet rather than a ban, so a project that
//! needs it for FFI can still adopt the gate.

use std::collections::BTreeMap;

use syn::visit::Visit;

use crate::gates::metrics::prodlines::{for_each_source, keyed, parse_rust};
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "unsafety",
    about: "`unsafe` outside test code, counted per file and shape",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "unsafe site(s)",
    },
};

/// Keyed by file and shape, never by line, so an edit above a site does not read as new debt.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    members_only(&ctx.root, for_each_source(ctx, &sites)?).map(keyed)
}

fn members_only<T>(
    root: &std::path::Path,
    read: Vec<(String, T)>,
) -> Result<Vec<(String, T)>, String> {
    let excluded = excluded(root)?;
    Ok(read
        .into_iter()
        .filter(|(shown, _)| {
            !excluded
                .iter()
                .any(|dir| crate::project::under(shown, dir).is_some())
        })
        .collect())
}

/// Crates listed in `[workspace] exclude`. They build on their own, often to confine FFI, so their
/// `unsafe` is not charged to the workspace.
fn excluded(root: &std::path::Path) -> Result<Vec<String>, String> {
    match std::fs::read_to_string(root.join(crate::project::MANIFEST)) {
        Ok(text) => crate::gates::cargo::profile::excluded_members(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", crate::project::MANIFEST)),
    }
}

/// Every `unsafe` site by shape: block, fn, trait, impl, and an `extern` block not marked `unsafe`,
/// which is implicitly unsafe. A `#[cfg(test)]` module is skipped.
fn sites(src: &str) -> Result<BTreeMap<&'static str, u64>, String> {
    let file = parse_rust(src)?;
    let mut count = Count {
        sites: BTreeMap::new(),
    };
    count.visit_file(&file);
    Ok(count.sites)
}

/// Sites counted per shape rather than summed, so a finding says what kind of `unsafe` to look at.
struct Count {
    sites: BTreeMap<&'static str, u64>,
}

impl Count {
    fn note(&mut self, shape: &'static str) {
        *self.sites.entry(shape).or_default() += 1;
    }
}

impl<'ast> Visit<'ast> for Count {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if crate::gates::metrics::prodlines::is_test_gated(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.note("block");
        syn::visit::visit_expr_unsafe(self, node);
    }

    fn visit_signature(&mut self, node: &'ast syn::Signature) {
        if matches!(node.safety, syn::Safety::Unsafe(_)) {
            self.note("fn");
        }
        syn::visit::visit_signature(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if node.unsafety.is_some() {
            self.note("trait");
        }
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.unsafety.is_some() {
            self.note("impl");
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast syn::ItemForeignMod) {
        if node.unsafety.is_none() {
            self.note("extern");
        }
        syn::visit::visit_item_foreign_mod(self, node);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn count(src: &str) -> u64 {
        sites(src).unwrap().values().sum()
    }

    fn shapes(src: &str) -> BTreeMap<&'static str, u64> {
        sites(src).unwrap()
    }

    #[test]
    fn a_file_with_no_unsafe_in_it_counts_none() {
        assert_eq!(count("fn f() -> u8 { 1 }\n"), 0);
    }

    #[test]
    fn an_unsafe_block_and_an_unsafe_function_each_count_once() {
        assert_eq!(count("fn f() { unsafe { g() } }\n"), 1);
        assert_eq!(count("unsafe fn f() {}\n"), 1);
        assert_eq!(count("unsafe fn f() { unsafe { g() } }\n"), 2);
    }

    #[test]
    fn an_unsafe_trait_and_an_unsafe_impl_each_count_once() {
        assert_eq!(count("unsafe trait T {}\n"), 1);
        assert_eq!(count("unsafe impl Send for S {}\n"), 1);
    }

    #[test]
    fn an_extern_block_counts_unless_it_is_declared_unsafe() {
        assert_eq!(count("extern \"C\" { fn g(); }\n"), 1);
        assert_eq!(count("unsafe extern \"C\" { fn g(); }\n"), 0);
    }

    #[test]
    fn unsafe_inside_a_test_module_is_the_harness_and_is_not_counted() {
        assert_eq!(
            count("#[cfg(test)]\nmod tests {\n    fn f() { unsafe { g() } }\n}\n"),
            0
        );
    }

    #[test]
    fn unsafe_beside_a_test_module_is_still_counted() {
        assert_eq!(
            count(
                "fn p() { unsafe { g() } }\n#[cfg(test)]\nmod tests {\n    fn f() { unsafe { g() } }\n}\n"
            ),
            1
        );
    }

    #[test]
    fn each_shape_is_counted_under_its_own_name() {
        let src = "unsafe impl Send for S {}\nfn f() { unsafe { g() } }\nunsafe fn h() {}\n";
        let found = shapes(src);
        assert_eq!(found.get("impl"), Some(&1));
        assert_eq!(found.get("block"), Some(&1));
        assert_eq!(found.get("fn"), Some(&1));
        assert_eq!(found.get("trait"), None);
    }

    #[test]
    fn a_key_names_the_file_and_the_shape_and_never_a_line() {
        let read = vec![
            (
                "src/a.rs".to_string(),
                shapes("fn f() { unsafe { g() } }\n"),
            ),
            (
                "src/b.rs".to_string(),
                shapes("unsafe impl Send for S {}\n"),
            ),
        ];
        let series = keyed(read);
        assert_eq!(series.get("src/a.rs#block"), Some(1));
        assert_eq!(series.get("src/b.rs#impl"), Some(1));
        assert_eq!(series.get("src/a.rs#impl"), None);
    }

    #[test]
    fn a_file_with_no_unsafe_in_it_contributes_no_key() {
        let read = vec![("src/clean.rs".to_string(), shapes("fn f() {}\n"))];
        assert_eq!(keyed(read), Series::new());
    }

    #[test]
    fn a_file_the_parser_rejects_stops_the_gate_rather_than_counting_zero() {
        assert!(sites("fn f( {\n").is_err());
    }

    fn workspace(name: &str, exclude: &str) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        let files = [
            (
                "Cargo.toml",
                format!("[workspace]\nmembers = [\"app\"]\n{exclude}"),
            ),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n".to_string()),
            ("app/src/lib.rs", "pub fn f() {}\n".to_string()),
            ("ffi/Cargo.toml", "[package]\nname = \"ffi\"\n".to_string()),
            ("ffi/src/lib.rs", "pub unsafe fn raw() {}\n".to_string()),
        ];
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn a_crate_the_workspace_excludes_is_not_charged_to_it() {
        let kept = workspace("unsafety-excluded", "exclude = [\"ffi/\"]\n");
        let ctx = Ctx::for_root(
            kept.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        assert_eq!(measure(&ctx).unwrap().0, BTreeMap::new());
        let member = workspace("unsafety-member", "");
        let ctx = Ctx::for_root(
            member.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        assert_eq!(
            measure(&ctx).unwrap().0,
            BTreeMap::from([("ffi/src/lib.rs#fn".to_string(), 1)])
        );
    }

    #[test]
    fn no_manifest_excludes_nothing_and_an_unreadable_one_refuses() {
        let bare = crate::testdir::make("unsafety-no-manifest");
        assert_eq!(excluded(&bare), Ok(Vec::new()));
        let odd = crate::testdir::make("unsafety-manifest-dir");
        std::fs::create_dir_all(odd.join("Cargo.toml")).unwrap();
        let refused = excluded(&odd).unwrap_err();
        assert!(refused.starts_with("Cargo.toml: "), "{refused}");
    }
}
