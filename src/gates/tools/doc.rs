//! The documentation check: one `cargo doc` for the workspace, or one per package when two packages
//! share an output name, which cargo cannot document in one run.

use std::collections::BTreeMap;
use std::path::Path;

use crate::project::workspace::{Metadata, Package, Target};

use crate::exec;
use crate::run::{Ctx, Outcome};

pub(super) fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let scopes = scopes(&crate::project::metadata(&ctx.root)?)?;
    let asked: Vec<String> = ctx.features.iter().chain(&ctx.build).cloned().collect();
    checked(&ctx.root, &scopes, &asked, &|args| {
        exec::run_env("cargo", args, &ctx.root, &[("RUSTDOCFLAGS", "-D warnings")])
            .map_err(|e| e.to_string())
    })
}

fn scopes(metadata: &str) -> Result<Vec<Option<String>>, String> {
    let metadata: Metadata = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo documentation metadata is unreadable: {e}"))?;
    let packages: Vec<&Package> = metadata
        .packages
        .iter()
        .filter(|p| metadata.workspace_members.contains(&p.id))
        .collect();
    let mut owners = BTreeMap::new();
    let mut collision = false;
    for package in &packages {
        for target in package.targets.iter().filter(|t| documents(t)) {
            let name = target.name.replace('-', "_");
            if let Some(owner) = owners.insert(name, &package.id) {
                collision |= owner != &package.id;
            }
        }
    }
    Ok(if collision {
        packages.iter().map(|p| Some(p.id.clone())).collect()
    } else {
        vec![None]
    })
}

fn documents(target: &Target) -> bool {
    target.doc
        && target.kind.iter().any(|kind| {
            matches!(
                kind.as_str(),
                "bin" | "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
            )
        })
}

fn invocation<'a>(package: Option<&'a str>, features: &'a [String]) -> Vec<&'a str> {
    let mut args = vec!["doc", "--no-deps"];
    match package {
        Some(package) => args.extend(["-p", package]),
        None => args.push("--workspace"),
    }
    args.extend(features.iter().map(String::as_str));
    args
}

fn checked(
    root: &Path,
    scopes: &[Option<String>],
    features: &[String],
    run: &dyn Fn(&[&str]) -> Result<exec::Output, String>,
) -> Result<Outcome, String> {
    let mut findings = Vec::new();
    for scope in scopes {
        let out = run(&invocation(scope.as_deref(), features))?;
        if out.truncated {
            return Err(
                "documentation output was truncated, so not every diagnostic was read".into(),
            );
        }
        let outcome = super::verdict(&out, root);
        if !outcome.passed {
            return Ok(outcome);
        }
        findings.extend(outcome.findings);
    }
    Ok(Outcome::noted(findings))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn metadata(second: &str) -> String {
        format!(
            r#"{{"workspace_members":["first","second"],"packages":[{{"id":"first","targets":[{{"name":"same-name","kind":["bin"]}}]}},{{"id":"second","targets":[{second}]}}]}}"#
        )
    }

    #[test]
    fn duplicate_output_names_are_documented_in_separate_package_invocations() {
        let same = metadata(r#"{"name":"same_name","kind":["bin"]}"#);
        assert_eq!(
            scopes(&same).unwrap(),
            [Some("first".into()), Some("second".into())]
        );
        let other = metadata(r#"{"name":"different","kind":["lib"]}"#);
        assert_eq!(scopes(&other).unwrap(), [None]);
        let excluded = metadata(r#"{"name":"same-name","kind":["bin"],"doc":false}"#);
        assert_eq!(scopes(&excluded).unwrap(), [None]);
        let test = metadata(r#"{"name":"same-name","kind":["test"]}"#);
        assert_eq!(scopes(&test).unwrap(), [None]);
        assert!(scopes("not json").is_err());
    }

    #[test]
    fn documentation_scopes_keep_all_feature_arguments() {
        let flags = ["--features".to_string(), "testkit".to_string()];
        assert_eq!(
            invocation(None, &flags),
            ["doc", "--no-deps", "--workspace", "--features", "testkit"]
        );
        assert_eq!(
            invocation(Some("first"), &flags),
            ["doc", "--no-deps", "-p", "first", "--features", "testkit"]
        );
    }

    #[test]
    fn every_package_is_checked_and_a_failure_never_becomes_a_partial_pass() {
        let scopes = [Some("first".into()), Some("second".into())];
        let calls = std::cell::RefCell::new(Vec::new());
        let run = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            Ok(exec::Output {
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                truncated: false,
            })
        };
        assert_eq!(
            checked(Path::new("/w"), &scopes, &[], &run).unwrap(),
            Outcome::passed()
        );
        assert_eq!(
            *calls.borrow(),
            ["doc --no-deps -p first", "doc --no-deps -p second"]
        );
        let fail = |_: &[&str]| {
            Ok(exec::Output {
                code: Some(1),
                stdout: String::new(),
                stderr: "error: fixture failed".into(),
                truncated: false,
            })
        };
        assert!(
            !checked(Path::new("/w"), &scopes, &[], &fail)
                .unwrap()
                .passed
        );
        let cut = |_: &[&str]| {
            Ok(exec::Output {
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                truncated: true,
            })
        };
        assert!(
            checked(Path::new("/w"), &scopes, &[], &cut)
                .unwrap_err()
                .contains("truncated")
        );
        assert!(
            checked(Path::new("/w"), &scopes, &[], &|_| Err(
                "spawn failed".into()
            ))
            .is_err()
        );
    }
}
