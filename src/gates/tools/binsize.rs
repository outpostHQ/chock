//! How large the shipped binaries are, and where their bytes go.

use serde::Deserialize;

use crate::project::workspace::{Metadata, Package, Target};

use crate::exec;
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "binsize",
    about: "the size of each release binary, in bytes",
    group: Group::Quality,
    builds: true,
    // A release build per package is worth recalling; its inputs are the tree and the toolchain.
    reads: Some(crate::run::verdicts::Reads::tree_and(&["cargo"]).versioned("targets-v1")),
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Sizes {
            basis_points: TOLERANCE_BP,
            floor: TOLERANCE_FLOOR,
        },
        unit: "byte(s)",
    },
};

/// What `--message-format=json` says about one artifact, of the fields this gate reads.
#[derive(Deserialize)]
struct Artifact {
    package_id: String,
    executable: Option<String>,
    target: Target,
}

/// A diagnostic in the same stream. With JSON output, compiler errors arrive here, not on stderr.
#[derive(Deserialize)]
struct Message {
    reason: String,
    message: Diagnostic,
}

#[derive(Deserialize)]
struct Diagnostic {
    level: String,
    message: String,
}

impl Target {
    fn binary(&self) -> bool {
        self.kind.iter().any(|kind| kind == "bin")
    }
}

/// A binary may grow by 1% or 256 KiB, whichever is larger, since almost every commit adds bytes.
const TOLERANCE_BP: u64 = 100;
const TOLERANCE_FLOOR: u64 = 256 * 1024;

/// The exact size of each shipped binary; the tolerance applies only when comparing.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let packages = shipped(&project::metadata(&ctx.root)?, &ctx.not_shipped)?;
    union_of(&packages, &|package| build(ctx, package))
}

/// Every package's binary sizes in one series, a name two packages share qualified by its package.
fn union_of(
    packages: &[Package],
    build: &dyn Fn(&Package) -> Result<Series, String>,
) -> Result<Series, String> {
    let mut sizes = Series::new();
    for package in packages {
        for (name, bytes) in build(package)?.0 {
            sizes.set(&binary_key(packages, package, &name), bytes);
        }
    }
    Ok(sizes)
}

fn binary_key(packages: &[Package], owner: &Package, binary: &str) -> String {
    let shared = packages.iter().any(|package| {
        package.id != owner.id && package.targets.iter().any(|target| target.name == binary)
    });
    if shared {
        format!("{}::{binary}", owner.name)
    } else {
        binary.to_string()
    }
}

/// One package's release build, or its named profile, with a `--bin` per target. Built per package
/// so workspace feature unification cannot change its size.
fn build_args<'a>(
    package: &'a Package,
    features: &'a [String],
    build: &'a [String],
) -> Vec<&'a str> {
    let mut args = vec!["build", "-p", &package.id, "--message-format=json"];
    if !build.iter().any(|flag| flag == "--profile") {
        args.push("--release");
    }
    for target in &package.targets {
        args.extend(["--bin", target.name.as_str()]);
    }
    args.extend(features.iter().map(String::as_str));
    args.extend(build.iter().map(String::as_str));
    args
}

fn build(ctx: &Ctx, package: &Package) -> Result<Series, String> {
    let out = exec::run(
        "cargo",
        &build_args(package, &ctx.features, &ctx.build),
        &ctx.root,
    )
    .map_err(|e| e.to_string())?;
    read_build(&out, package, &|path| {
        std::fs::metadata(path).map(|m| m.len()).ok()
    })
}

/// The sizes from one build's output; a failed build or a truncated stream is an error.
fn read_build(
    out: &exec::Output,
    package: &Package,
    size_of: &dyn Fn(&str) -> Option<u64>,
) -> Result<Series, String> {
    if !out.success() {
        let why = first_error(&out.stdout).unwrap_or_else(|| out.why_it_failed().to_string());
        return Err(format!(
            "the release build of {} failed, so there is nothing to size: {why}",
            package.name
        ));
    }
    if out.truncated {
        return Err(format!(
            "cargo printed more about {} than chock keeps; the sizes would be partial",
            package.name
        ));
    }
    sizes(&out.stdout, package, size_of)
}

/// Workspace members with a `[[bin]]` target, except those the project's config marks unshipped.
fn shipped(metadata: &str, not_shipped: &[String]) -> Result<Vec<Package>, String> {
    let (root, all) = binary_members(metadata)?;
    Ok(all
        .into_iter()
        .filter(|package| receives(&root, package, not_shipped))
        .collect())
}

/// Every workspace member with a `[[bin]]` target, sorted, and the workspace root.
fn binary_members(metadata: &str) -> Result<(std::path::PathBuf, Vec<Package>), String> {
    let read: Metadata = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    let mut named: Vec<Package> = read
        .packages
        .into_iter()
        .filter(|package| read.workspace_members.contains(&package.id))
        .filter_map(|mut package| {
            package.targets.retain(Target::binary);
            (!package.targets.is_empty()).then_some(package)
        })
        .collect();
    named.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((std::path::PathBuf::from(read.workspace_root), named))
}

fn receives(root: &std::path::Path, package: &Package, not_shipped: &[String]) -> bool {
    let manifest = project::relative(root, std::path::Path::new(&package.manifest_path));
    project::ships(&manifest, not_shipped)
}

/// Where each shipped binary's bytes go, as `cargo bsize` reports it.
pub(super) fn bsize(ctx: &Ctx) -> Result<Outcome, String> {
    let named = shipped_binaries(&project::metadata(&ctx.root)?, &ctx.not_shipped)?;
    let said = each_binary(ctx, &named)?;
    Ok(measured_sizes(&named, &said))
}

/// One `cargo bsize` per binary, because the tool reports one and refuses a workspace of several.
fn each_binary(ctx: &Ctx, named: &[String]) -> Result<Vec<String>, String> {
    named.iter().map(|binary| instrument(ctx, binary)).collect()
}

fn instrument(ctx: &Ctx, binary: &str) -> Result<String, String> {
    ctx.default_build("cargo bsize")?;
    let args = bsize_args(binary, &ctx.features)?;
    let out = exec::tool(&ctx.root, "cargo", &args)?;
    reported(binary, &out)
}

fn bsize_args<'a>(binary: &'a str, features: &[String]) -> Result<Vec<&'a str>, String> {
    if !features.is_empty() {
        return Err(format!(
            "cargo bsize cannot size `{binary}` with the configured feature selection: this tool has no feature flags; use binsize for that build"
        ));
    }
    Ok(vec!["bsize", "--bin", binary])
}

/// One binary's report, or the tool's own words for why there is none.
fn reported(binary: &str, out: &exec::Output) -> Result<String, String> {
    if !out.success() {
        return Err(format!(
            "cargo bsize could not size `{binary}`: {}",
            out.why_it_failed()
        ));
    }
    complete_output(binary, out)
}

fn complete_output(binary: &str, out: &exec::Output) -> Result<String, String> {
    if out.truncated || out.stdout.trim().is_empty() {
        return Err(format!(
            "cargo bsize reported no complete output for `{binary}`"
        ));
    }
    Ok(out.stdout.clone())
}

/// A pass carrying the reports; a project that ships no binary passes with nothing to show.
fn measured_sizes(named: &[String], said: &[String]) -> Outcome {
    let mut outcome = Outcome::passed();
    if named.is_empty() {
        return outcome;
    }
    outcome.said = Some(said.join("\n"));
    outcome
}

/// Every `[[bin]]` target a shipped member builds, by the name cargo knows it as.
pub(super) fn shipped_binaries(
    metadata: &str,
    not_shipped: &[String],
) -> Result<Vec<String>, String> {
    let (root, all) = binary_members(metadata)?;
    let selected: Vec<&Package> = all
        .iter()
        .filter(|package| receives(&root, package, not_shipped))
        .collect();
    instrument_names(&all, &selected)
}

fn instrument_names(all: &[Package], selected: &[&Package]) -> Result<Vec<String>, String> {
    let mut named = selected
        .iter()
        .copied()
        .flat_map(|package| package.targets.iter().map(move |target| (package, target)))
        .map(|(package, target)| instrument_name(all, package, target))
        .collect::<Result<Vec<_>, _>>()?;
    named.sort();
    Ok(named)
}

fn instrument_name(all: &[Package], package: &Package, target: &Target) -> Result<String, String> {
    let owners: Vec<&Package> = all
        .iter()
        .filter(|candidate| candidate.targets.iter().any(|bin| bin.name == target.name))
        .collect();
    if owners.iter().any(|owner| owner.id != package.id) {
        let names: Vec<&str> = owners.iter().map(|owner| owner.name.as_str()).collect();
        return Err(format!(
            "cargo bsize cannot disambiguate binary `{}` in packages {}; this tool has no package selector",
            target.name,
            names.join(", ")
        ));
    }
    Ok(target.name.clone())
}

/// The first error the compiler reported, out of the JSON this gate asked cargo for.
fn first_error(json: &str) -> Option<String> {
    json.lines()
        .filter_map(|line| serde_json::from_str::<Message>(line).ok())
        .find(|held| held.reason == "compiler-message" && held.message.level == "error")
        .map(|held| held.message.message)
}

/// The size of each binary the build reported for `package`. A named file that cannot be sized is
/// an error, never zero.
fn sizes(
    json: &str,
    package: &Package,
    size_of: &dyn Fn(&str) -> Option<u64>,
) -> Result<Series, String> {
    let series = json.lines().try_fold(Series::new(), |mut series, line| {
        if let Some(artifact) = owned_artifact(line, package)? {
            record_size(&mut series, artifact, package, size_of)?;
        }
        Ok::<_, String>(series)
    })?;
    all_built(&series, package).map(|()| series)
}

fn owned_artifact(line: &str, package: &Package) -> Result<Option<Artifact>, String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(None);
    };
    if value["reason"] != "compiler-artifact" {
        return Ok(None);
    }
    let artifact: Artifact = serde_json::from_value(value)
        .map_err(|e| format!("cargo reported an unreadable artifact: {e}"))?;
    Ok((artifact.package_id == package.id && artifact.target.binary()).then_some(artifact))
}

fn all_built(series: &Series, package: &Package) -> Result<(), String> {
    let missing: Vec<&str> = package
        .targets
        .iter()
        .filter(|target| series.get(&target.name).is_none())
        .map(|target| target.name.as_str())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the release build of {} produced no executable for: {}",
        package.name,
        missing.join(", ")
    ))
}

fn record_size(
    series: &mut Series,
    artifact: Artifact,
    package: &Package,
    size_of: &dyn Fn(&str) -> Option<u64>,
) -> Result<(), String> {
    let name = &artifact.target.name;
    if !package.targets.iter().any(|target| target.name == *name) || series.get(name).is_some() {
        return Err(format!(
            "cargo reported an unexpected or repeated binary `{name}` for {}",
            package.name
        ));
    }
    let path = artifact
        .executable
        .ok_or_else(|| format!("cargo reported no executable for {}::{name}", package.name))?;
    let bytes =
        size_of(&path).ok_or_else(|| format!("cargo named {path} but it is not there to size"))?;
    series.set(name, bytes);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn package_names(metadata: &str, excluded: &[String]) -> Vec<String> {
        shipped(metadata, excluded)
            .unwrap()
            .into_iter()
            .map(|package| package.name)
            .collect()
    }

    fn package(name: &str, bins: &[&str]) -> Package {
        Package {
            id: name.to_string(),
            name: name.to_string(),
            manifest_path: format!("/w/{name}/Cargo.toml"),
            targets: bins
                .iter()
                .map(|name| Target {
                    name: (*name).to_string(),
                    kind: vec!["bin".to_string()],
                    ..Target::default()
                })
                .collect(),
            ..Package::default()
        }
    }

    fn tool_package() -> Package {
        package("cli", &["tool"])
    }

    fn artifact(owner: &str, name: &str, kind: &str) -> String {
        serde_json::json!({
            "reason": "compiler-artifact", "package_id": owner,
            "executable": format!("/t/release/{name}"),
            "target": {"name": name, "kind": [kind]}
        })
        .to_string()
    }

    #[test]
    fn private_binaries_are_measured_unless_the_project_explicitly_excludes_them() {
        let fuzzy = r#"{"workspace_root": "/w",
          "workspace_members": ["app 1.0.0", "fuzz 1.0.0", "bench 1.0.0"],
          "packages": [
            {"id": "app 1.0.0", "name": "app", "manifest_path": "/w/crates/app/Cargo.toml",
             "targets": [{"name": "app", "kind": ["bin"]}]},
            {"id": "fuzz 1.0.0", "name": "fuzz", "manifest_path": "/w/crates/fuzz/Cargo.toml",
             "publish": [],
             "targets": [{"name": "fuzz_one", "kind": ["bin"]}, {"name": "fuzz_two", "kind": ["bin"]}]},
            {"id": "bench 1.0.0", "name": "bench", "manifest_path": "/w/crates/bench/Cargo.toml",
             "publish": [], "targets": [{"name": "load_test", "kind": ["bin"]}]}]}"#;
        assert_eq!(
            shipped_binaries(fuzzy, &[]).unwrap(),
            ["app", "fuzz_one", "fuzz_two", "load_test"]
        );
        assert_eq!(package_names(fuzzy, &[]), ["app", "bench", "fuzz"]);
        let excluded = ["crates/fuzz".to_string(), "crates/bench".to_string()];
        assert_eq!(shipped_binaries(fuzzy, &excluded).unwrap(), ["app"]);
        assert_eq!(package_names(fuzzy, &excluded), ["app"]);
    }

    #[test]
    fn a_member_published_to_a_named_registry_is_still_shipped() {
        let private = r#"{"workspace_root": "/w", "workspace_members": ["app 1.0.0"],
          "packages": [{"id": "app 1.0.0", "name": "app", "manifest_path": "/w/Cargo.toml",
            "publish": ["our-registry"], "targets": [{"name": "app", "kind": ["bin"]}]}]}"#;
        assert_eq!(
            shipped_binaries(private, &[]).unwrap(),
            vec!["app".to_string()]
        );
    }

    #[test]
    fn a_tool_that_refused_to_size_a_binary_is_a_gate_that_could_not_run() {
        let refused = exec::Output::of(
            Some(2),
            "",
            "error: workspace has several bin targets, pick one with --bin: app, appd\n",
        );
        let why = reported("app", &refused).unwrap_err();
        assert!(why.contains("could not size `app`"), "{why}");
        assert!(why.contains("several bin targets"), "{why}");
    }

    #[test]
    fn what_the_instrument_said_about_each_binary_is_carried_back() {
        let sized = exec::Output::of(Some(0), "app 4.2 MiB\n", "");
        assert_eq!(reported("app", &sized).unwrap(), "app 4.2 MiB\n");
        let outcome = measured_sizes(
            &["app".to_string(), "appd".to_string()],
            &["app 4.2 MiB".to_string(), "appd 1.1 MiB".to_string()],
        );
        assert!(outcome.passed);
        assert_eq!(outcome.said.as_deref(), Some("app 4.2 MiB\nappd 1.1 MiB"));
    }

    #[test]
    fn a_project_that_ships_no_binary_measures_none_rather_than_refusing() {
        let outcome = measured_sizes(&[], &[]);
        assert!(outcome.passed);
        assert_eq!(outcome.said, None);
    }

    const BUILT: &str = r#"{"reason":"compiler-artifact","package_id":"cli","executable":"/t/release/tool","target":{"name":"tool","kind":["bin"]}}
{"reason":"compiler-artifact","package_id":"cli","executable":null,"target":{"name":"lib","kind":["lib"]}}
{"reason":"build-finished","success":true}"#;

    const WORKSPACE: &str = r#"{"workspace_root": "/w", "workspace_members": ["cli 0.1.0", "lib 0.1.0", "fuzz 0.1.0"], "packages": [{"id": "cli 0.1.0", "name": "cli", "manifest_path": "/w/crates/cli/Cargo.toml", "targets": [{"name": "cli", "kind": ["bin"]}, {"name": "cli", "kind": ["lib"]}]}, {"id": "lib 0.1.0", "name": "lib", "manifest_path": "/w/crates/lib/Cargo.toml", "targets": [{"name": "lib", "kind": ["lib"]}]}, {"id": "fuzz 0.1.0", "name": "fuzz", "manifest_path": "/w/conformance/fuzz/Cargo.toml", "targets": [{"name": "fuzz", "kind": ["bin"]}]}, {"id": "other 0.1.0", "name": "other", "manifest_path": "/w/other/Cargo.toml", "targets": [{"name": "other", "kind": ["bin"]}]}]}"#;

    #[test]
    fn every_workspace_member_that_builds_a_binary_is_sized_on_its_own() {
        assert_eq!(
            package_names(WORKSPACE, &[]),
            vec!["cli".to_string(), "fuzz".to_string()]
        );
    }

    #[test]
    fn every_bin_target_a_shipped_member_builds_is_named_for_the_instrument() {
        assert_eq!(
            shipped_binaries(WORKSPACE, &[]).unwrap(),
            vec!["cli".to_string(), "fuzz".to_string()]
        );
        // Library targets and members marked not shipped are left out.
        assert_eq!(
            shipped_binaries(WORKSPACE, &["conformance/**".to_string()]).unwrap(),
            vec!["cli".to_string()]
        );
    }

    #[test]
    fn a_member_the_project_says_nobody_receives_is_not_built_or_sized() {
        assert_eq!(
            package_names(WORKSPACE, &["conformance/**".to_string()]),
            vec!["cli".to_string()]
        );
    }

    #[test]
    fn a_workspace_whose_members_are_all_libraries_sizes_nothing_rather_than_failing() {
        assert_eq!(
            package_names(
                WORKSPACE,
                &["conformance/**".to_string(), "crates/cli".to_string()]
            ),
            Vec::<String>::new()
        );
    }

    /// Each executable is keyed by its target name, not by the file cargo wrote.
    #[test]
    fn every_executable_one_build_emits_lands_in_one_series() {
        let two = [
            artifact("cli", "outpost-cli", "bin"),
            artifact("cli", "outpost-server", "bin"),
        ]
        .join("\n");
        let owner = package("cli", &["outpost-cli", "outpost-server"]);
        let series = sizes(&two, &owner, &|path| match path {
            "/t/release/outpost-cli" => Some(1_000),
            "/t/release/outpost-server" => Some(2_000),
            _ => None,
        })
        .unwrap();
        assert_eq!(series.get("outpost-cli"), Some(1_000));
        assert_eq!(series.get("outpost-server"), Some(2_000));
    }

    #[test]
    fn a_build_that_failed_or_was_cut_short_sizes_nothing() {
        let failed = exec::Output::of(
            Some(101),
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"no method `f`\"}}\n",
            "   Compiling proc-macro2 v1.0.107\n",
        );
        assert_eq!(
            read_build(&failed, &tool_package(), &|_| Some(9)).unwrap_err(),
            "the release build of cli failed, so there is nothing to size: no method `f`"
        );
        let cut = exec::Output {
            code: Some(0),
            stdout: BUILT.to_string(),
            stderr: String::new(),
            truncated: true,
        };
        assert_eq!(
            read_build(&cut, &tool_package(), &|_| Some(9)).unwrap_err(),
            "cargo printed more about cli than chock keeps; the sizes would be partial"
        );
        let built = exec::Output::of(Some(0), BUILT, "");
        assert_eq!(
            read_build(&built, &tool_package(), &|_| Some(9))
                .unwrap()
                .get("tool"),
            Some(9)
        );
    }

    #[test]
    fn the_reason_a_build_failed_is_read_from_the_json_cargo_was_asked_for() {
        let stream = "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"unused import\"}}\n\
                      {\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"cannot find value `x` in this scope\"}}\n";
        assert_eq!(
            first_error(stream),
            Some("cannot find value `x` in this scope".to_string())
        );
        assert_eq!(first_error("{\"reason\":\"build-finished\"}\n"), None);
    }

    /// The executable is the last line here.
    #[test]
    fn a_line_with_nothing_to_size_is_stepped_over_rather_than_ending_the_read() {
        let trailing = format!(
            "{{\"reason\":\"build-finished\",\"success\":true}}\n{}\n{}",
            artifact("cli", "build-script-build", "custom-build"),
            artifact("cli", "tool", "bin")
        );
        let series = sizes(&trailing, &tool_package(), &|path| {
            (path == "/t/release/tool").then_some(4096)
        })
        .unwrap();
        assert_eq!(series.get("tool"), Some(4096));
    }

    #[test]
    fn every_executable_cargo_named_is_sized_and_a_library_is_not() {
        let series = sizes(BUILT, &tool_package(), &|path| {
            (path == "/t/release/tool").then_some(4096)
        })
        .unwrap();
        assert_eq!(series.get("tool"), Some(4096));
        assert_eq!(series.get("lib"), None);
    }

    fn series(of: &[(&str, u64)]) -> Series {
        let mut series = Series::new();
        for (name, bytes) in of {
            series.set(name, *bytes);
        }
        series
    }

    #[test]
    fn every_shipped_packages_binaries_land_in_one_series() {
        let all = union_of(
            &[
                package("cli", &["cli"]),
                package("helper", &["helper", "helper-aux"]),
            ],
            &|package| match package.name.as_str() {
                "cli" => Ok(series(&[("cli", 4096)])),
                _ => Ok(series(&[("helper", 8192), ("helper-aux", 512)])),
            },
        )
        .unwrap();
        assert_eq!(all.get("cli"), Some(4096));
        assert_eq!(all.get("helper"), Some(8192));
        assert_eq!(all.get("helper-aux"), Some(512));
    }

    #[test]
    fn a_package_whose_build_failed_stops_the_gate_rather_than_sizing_the_rest() {
        assert_eq!(
            union_of(
                &[package("cli", &["cli"]), package("helper", &["helper"])],
                &|package| {
                    match package.name.as_str() {
                        "cli" => Ok(series(&[("cli", 4096)])),
                        _ => Err(format!("the release build of {} failed", package.name)),
                    }
                }
            )
            .unwrap_err(),
            "the release build of helper failed"
        );
    }

    #[test]
    fn a_binary_may_grow_by_a_percent_or_a_quarter_mebibyte_before_it_trips() {
        let keys = match GATE.kind {
            Kind::Ratchet { keys, .. } => keys,
            _ => Keys::Items,
        };
        let was = series(&[("tool", 3_105_936)]);
        assert_eq!(
            series(&[("tool", 3_105_936 + 256 * 1024)]).regressions(&was, keys),
            Vec::new()
        );
        let big = 100 * 1024 * 1024;
        assert_eq!(keys.allowed(big), big + big / 100);
        assert_eq!(keys.allowed(3_105_936), 3_105_936 + 256 * 1024);
    }

    #[test]
    fn a_build_that_produced_no_binary_is_a_gate_that_could_not_run() {
        assert_eq!(
            sizes(
                r#"{"reason":"build-finished","success":true}"#,
                &tool_package(),
                &|_| Some(1)
            ),
            Err("the release build of cli produced no executable for: tool".to_string())
        );
    }

    #[test]
    fn an_artifact_that_is_not_on_disk_stops_the_gate_rather_than_sizing_as_zero() {
        assert_eq!(
            sizes(BUILT, &tool_package(), &|_| None),
            Err("cargo named /t/release/tool but it is not there to size".to_string())
        );
    }

    #[test]
    fn equally_named_binaries_keep_both_package_measurements() {
        let packages = [
            package("alpha", &["service"]),
            package("beta", &["service"]),
        ];
        let all = union_of(&packages, &|package| {
            Ok(series(&[(
                "service",
                if package.name == "alpha" { 11 } else { 22 },
            )]))
        })
        .unwrap();
        assert_eq!(
            all,
            series(&[("alpha::service", 11), ("beta::service", 22)])
        );
    }

    #[test]
    fn explicit_bin_arguments_preserve_feature_options_and_required_targets() {
        let owner = package("app", &["server", "admin"]);
        let features = [
            "--no-default-features".to_string(),
            "--features".to_string(),
            "admin".to_string(),
        ];
        assert_eq!(
            build_args(&owner, &features, &[]),
            [
                "build",
                "-p",
                "app",
                "--message-format=json",
                "--release",
                "--bin",
                "server",
                "--bin",
                "admin",
                "--no-default-features",
                "--features",
                "admin",
            ]
        );
        let shipped = ["--target", "wasm32-wasip1", "--profile", "dist"].map(String::from);
        assert_eq!(
            build_args(&owner, &[], &shipped),
            [
                "build",
                "-p",
                "app",
                "--message-format=json",
                "--bin",
                "server",
                "--bin",
                "admin",
                "--target",
                "wasm32-wasip1",
                "--profile",
                "dist",
            ],
            "a named profile replaces --release rather than fighting it"
        );
    }

    #[test]
    fn required_feature_targets_are_selected_even_under_the_default_feature_set() {
        let metadata = r#"{"workspace_root":"/w","workspace_members":["app"],"packages":[
            {"id":"app","name":"app","manifest_path":"/w/Cargo.toml","targets":[
              {"name":"admin","kind":["bin"],"required-features":["admin"]}]}]}"#;
        let packages = shipped(metadata, &[]).unwrap();
        assert_eq!(
            build_args(&packages[0], &[], &[]),
            [
                "build",
                "-p",
                "app",
                "--message-format=json",
                "--release",
                "--bin",
                "admin"
            ]
        );
    }

    #[test]
    fn a_partial_build_cannot_hide_a_binary_with_unselected_required_features() {
        let owner = package("cli", &["tool", "admin"]);
        assert_eq!(
            sizes(BUILT, &owner, &|_| Some(4)),
            Err("the release build of cli produced no executable for: admin".to_string())
        );
    }

    #[test]
    fn another_packages_binary_and_a_build_script_are_not_this_packages_executable() {
        let stream = [
            artifact("dependency", "tool", "bin"),
            artifact("cli", "tool", "custom-build"),
        ]
        .join("\n");
        assert_eq!(
            sizes(&stream, &tool_package(), &|_| Some(4)),
            Err("the release build of cli produced no executable for: tool".to_string())
        );
    }

    #[test]
    fn ambiguous_instrument_targets_are_refused_even_if_one_owner_is_excluded() {
        let metadata = r#"{"workspace_root":"/w","workspace_members":["alpha","beta"],"packages":[
            {"id":"alpha","name":"alpha","manifest_path":"/w/alpha/Cargo.toml","targets":[{"name":"service","kind":["bin"]}]},
            {"id":"beta","name":"beta","manifest_path":"/w/beta/Cargo.toml","targets":[{"name":"service","kind":["bin"]}]}]}"#;
        let expected = Err("cargo bsize cannot disambiguate binary `service` in packages alpha, beta; this tool has no package selector".to_string());
        assert_eq!(shipped_binaries(metadata, &[]), expected);
        assert_eq!(shipped_binaries(metadata, &["beta".to_string()]), expected);
    }

    #[test]
    fn an_instrument_without_feature_flags_cannot_ignore_the_requested_build() {
        assert_eq!(bsize_args("app", &[]).unwrap(), ["bsize", "--bin", "app"]);
        for features in [
            vec!["--all-features".to_string()],
            vec!["--features".to_string(), "testkit".to_string()],
        ] {
            assert_eq!(bsize_args("app", &features),
                Err("cargo bsize cannot size `app` with the configured feature selection: this tool has no feature flags; use binsize for that build".to_string()));
        }
    }

    #[test]
    fn repeated_or_unreadable_artifacts_never_overwrite_a_valid_size() {
        let repeated = format!("{BUILT}\n{}", artifact("cli", "tool", "bin"));
        assert_eq!(
            sizes(&repeated, &tool_package(), &|_| Some(4)),
            Err("cargo reported an unexpected or repeated binary `tool` for cli".to_string())
        );
        let bad = format!("{BUILT}\n{{\"reason\":\"compiler-artifact\"}}");
        assert!(
            sizes(&bad, &tool_package(), &|_| Some(4))
                .unwrap_err()
                .contains("unreadable artifact")
        );
        let unknown = artifact("cli", "unselected", "bin");
        assert_eq!(
            sizes(&unknown, &tool_package(), &|_| Some(4)),
            Err("cargo reported an unexpected or repeated binary `unselected` for cli".to_string())
        );
    }

    #[test]
    fn a_named_bin_artifact_without_an_executable_is_not_measured() {
        let missing = r#"{"reason":"compiler-artifact","package_id":"cli","executable":null,"target":{"name":"tool","kind":["bin"]}}"#;
        assert_eq!(
            sizes(missing, &tool_package(), &|_| Some(4)),
            Err("cargo reported no executable for cli::tool".to_string())
        );
    }

    #[test]
    fn a_successful_instrument_with_empty_or_truncated_output_measured_nothing() {
        for (stdout, truncated) in [("", false), ("partial", true)] {
            let out = exec::Output {
                code: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
                truncated,
            };
            assert_eq!(
                reported("app", &out),
                Err("cargo bsize reported no complete output for `app`".to_string())
            );
        }
    }

    #[test]
    fn a_line_that_is_not_an_artifact_is_passed_over() {
        let noisy = format!("   Compiling serde v1.0.0\n{BUILT}\nwarning: unused\n");
        let series = sizes(&noisy, &tool_package(), &|_| Some(10)).unwrap();
        assert_eq!(series.get("tool"), Some(10));
    }
}
