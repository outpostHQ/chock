//! The `workflows` gate: each GitHub Actions workflow and action runs code that its owner cannot
//! change after review, so a takeover of an action's tag or an image's name reaches no build.

use std::path::Path;

use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

use yaml::Node;

mod pin;
mod yaml;

/// The `workflows` gate: actions and images that a workflow names by a movable name.
pub const GATE: Gate = Gate {
    name: "workflows",
    about: "a GitHub Actions workflow or action runs an action or an image by a name that its owner \
            can move",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let mut found = Vec::new();
    for (shown, text) in ctx.texts(&read_by_github)? {
        found.extend(faults(&shown, &text));
    }
    Ok(if found.is_empty() {
        Outcome::passed()
    } else {
        Outcome::failed(found)
    })
}

/// Whether GitHub runs the file: a workflow directly in `.github/workflows`, or an action's
/// metadata file in any directory.
fn read_by_github(name: &str, path: &std::path::Path) -> bool {
    let yaml = Path::new(name).extension().is_some_and(|ext| ext == "yml")
        || Path::new(name).extension().is_some_and(|ext| ext == "yaml");
    let workflow = path
        .parent()
        .is_some_and(|dir| dir.ends_with(".github/workflows"));
    yaml && (workflow || matches!(name, "action.yml" | "action.yaml"))
}

/// Each name in one workflow or action file that its owner can move.
fn faults(path: &str, text: &str) -> Vec<Finding> {
    match yaml::documents(text) {
        Ok(documents) => documents
            .iter()
            .flat_map(names)
            .filter_map(|(names, value)| moved(names, value))
            .map(|(line, message)| Finding::at(path, &message).line(line))
            .collect(),
        Err(why) => vec![Finding::at(
            path,
            &format!("cannot be read as YAML, so its pins cannot be checked: {why}"),
        )],
    }
}

/// What a value names: an action or a reusable workflow, a container image, or the image of a
/// Docker action, which may also be a Dockerfile beside it.
#[derive(Clone, Copy)]
enum Names {
    Action,
    Image,
    ActionImage,
}

/// Each value in one document that names code to run.
fn names(document: &Node) -> Vec<(Names, &Node)> {
    let mut named = Vec::new();
    for (_, job) in document.get("jobs").map_or(&[][..], Node::entries) {
        named.extend(job.get("uses").map(|uses| (Names::Action, uses)));
        named.extend(
            job.get("container")
                .map(|container| (Names::Image, container.get("image").unwrap_or(container))),
        );
        let services = job.get("services").map_or(&[][..], Node::entries);
        named.extend(
            services
                .iter()
                .filter_map(|(_, service)| service.get("image"))
                .map(|image| (Names::Image, image)),
        );
        named.extend(steps(job));
    }
    if let Some(runs) = document.get("runs") {
        named.extend(steps(runs));
        named.extend(runs.get("image").map(|image| (Names::ActionImage, image)));
    }
    named
}

/// The `uses` of each step a job or a composite action runs.
fn steps(holder: &Node) -> impl Iterator<Item = (Names, &Node)> {
    let steps = holder.get("steps").map_or(&[][..], Node::items);
    steps
        .iter()
        .filter_map(|step| step.get("uses"))
        .map(|uses| (Names::Action, uses))
}

/// The line and the message for a value that its owner can move.
fn moved(names: Names, value: &Node) -> Option<(u32, String)> {
    let (text, line) = value.scalar()?;
    let bytes = text.as_bytes();
    let message = match names {
        Names::Action if pin::action(bytes) == pin::Pin::Movable => format!(
            "`{text}` runs code its owner can move: pin it to the full 40-character commit SHA \
             that the tag names now, and keep the tag in a comment"
        ),
        Names::Image | Names::ActionImage if text.contains("${{") => format!(
            "`{text}` names its image through an expression: chock cannot check that pin, so \
             name the image by its digest"
        ),
        Names::Image if !pin::image(bytes) => format!(
            "`{text}` names an image its owner can move: pin it by digest, as \
             `image@sha256:<64 hex digits>`"
        ),
        Names::ActionImage
            if bytes.starts_with(b"docker://") && pin::action(bytes) != pin::Pin::Digest =>
        {
            format!(
                "`{text}` names an image its owner can move: pin it by digest, as \
                 `docker://image@sha256:<64 hex digits>`"
            )
        }
        _ => return None,
    };
    Some((line, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "8e5e7e5ab8b370d6c329ec480221332ada57f0ab";
    const DIGEST: &str = "sha256:2b5e7e5ab8b370d6c329ec480221332ada57f0ab8e5e7e5ab8b370d6c329ec48";

    /// Each finding in a workflow as `line: message start`, the message cut before its fix.
    fn shown(text: &str) -> Vec<String> {
        faults(".github/workflows/ci.yml", text)
            .iter()
            .map(|finding| {
                let message = finding.message.split(": ").next().unwrap_or_default();
                format!("{}: {message}", finding.line.unwrap_or_default())
            })
            .collect()
    }

    #[test]
    fn a_step_or_a_job_that_runs_an_action_by_tag_is_a_finding_at_its_line() {
        let text = "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      \
                    - uses: actions/checkout@v4\n      - run: make\n  call:\n    \
                    uses: org/repo/.github/workflows/w.yml@main\n";
        assert_eq!(
            shown(text),
            [
                "6: `actions/checkout@v4` runs code its owner can move",
                "9: `org/repo/.github/workflows/w.yml@main` runs code its owner can move",
            ]
        );
    }

    #[test]
    fn a_uses_or_an_image_that_is_not_a_scalar_names_no_code_and_is_no_finding() {
        // GitHub refuses such a file, so it runs nothing.
        let text = "jobs:\n  build:\n    container:\n      image: [node]\n    steps:\n      \
                    - uses: [actions/checkout@v4]\n";
        assert_eq!(shown(text), Vec::<String>::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_trips_on_each_workflow_in_the_tree_that_runs_code_its_owner_can_move() {
        let moved = "jobs:\n  build:\n    steps:\n      - uses: actions/checkout@v4\n";
        let pinned = format!("jobs:\n  build:\n    steps:\n      - uses: actions/checkout@{SHA}\n");
        let files = [
            (".github/workflows/ci.yml", pinned.as_str()),
            ("docs/ci.yml", moved),
        ];
        let ctx = crate::testdir::Held::tree("gate-workflows", &files);
        assert_eq!(check(&ctx), Ok(Outcome::passed()));
        let files = [
            (".github/workflows/ci.yml", moved),
            (".github/workflows/ok.yml", pinned.as_str()),
        ];
        let ctx = crate::testdir::Held::tree("gate-workflows-moved", &files);
        let tripped = check(&ctx).map(|outcome| {
            let at = |f: &Finding| (f.file.clone(), f.line);
            (
                outcome.passed,
                outcome.findings.iter().map(at).collect::<Vec<_>>(),
            )
        });
        let ci = ".github/workflows/ci.yml".to_string();
        assert_eq!(tripped, Ok((false, vec![(ci, Some(4))])));
    }

    #[test]
    fn a_commit_sha_a_local_action_and_a_digest_pass() {
        let text = format!(
            "jobs:\n  build:\n    container:\n      image: node@{DIGEST}\n    services:\n      \
             db:\n        image: postgres@{DIGEST}\n    steps:\n      - uses: actions/checkout@{SHA}\n      \
             - uses: ./.github/actions/setup\n      - uses: $/.github/actions/setup\n      \
             - uses: docker://alpine@{DIGEST}\n  call:\n    uses: $/.github/workflows/build.yml\n"
        );
        assert_eq!(shown(&text), Vec::<String>::new());
    }

    #[test]
    fn a_container_or_a_service_image_by_tag_is_a_finding() {
        let text = "jobs:\n  a:\n    container: node:20\n  b:\n    container:\n      \
                    image: node:20\n    services:\n      db:\n        image: postgres\n";
        assert_eq!(
            shown(text),
            [
                "3: `node:20` names an image its owner can move",
                "6: `node:20` names an image its owner can move",
                "9: `postgres` names an image its owner can move",
            ]
        );
    }

    #[test]
    fn an_action_file_is_read_for_its_steps_and_its_docker_image() {
        let composite = "runs:\n  using: composite\n  steps:\n    - uses: actions/cache@v4\n";
        let docker = "runs:\n  using: docker\n  image: docker://alpine:3\n";
        let local = "runs:\n  using: docker\n  image: Dockerfile\n";
        let lines = |text: &str| {
            faults("action.yml", text)
                .iter()
                .map(|finding| finding.line.unwrap_or_default())
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(composite), [4]);
        assert_eq!(lines(docker), [3]);
        assert_eq!(lines(local), Vec::<u32>::new());
    }

    #[test]
    fn an_alias_reports_the_line_that_uses_it() {
        let text = "x: &checkout actions/checkout@v4\njobs:\n  a:\n    steps:\n      \
                    - uses: *checkout\n";
        assert_eq!(
            shown(text),
            ["5: `actions/checkout@v4` runs code its owner can move"]
        );
    }

    #[test]
    fn an_image_named_by_an_expression_cannot_be_checked_and_is_a_finding() {
        let text = "jobs:\n  a:\n    container: ${{ matrix.image }}\n";
        assert_eq!(
            shown(text),
            ["3: `${{ matrix.image }}` names its image through an expression"]
        );
    }

    #[test]
    fn a_file_that_is_not_yaml_is_a_finding_rather_than_a_pass() {
        assert_eq!(
            shown("jobs: [unclosed\n"),
            ["0: cannot be read as YAML, so its pins cannot be checked"]
        );
    }

    #[test]
    fn github_reads_workflows_from_one_directory_and_action_files_from_any() {
        let read = |path: &str| {
            let path = std::path::Path::new(path);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            read_by_github(name, path)
        };
        assert!(read(".github/workflows/ci.yml"));
        assert!(read(".github/workflows/release.yaml"));
        assert!(read("action.yml"));
        assert!(read("tools/setup/action.yaml"));
        assert!(!read(".github/workflows/nested/ci.yml"));
        assert!(!read(".github/dependabot.yml"));
        assert!(!read("config/action.json"));
    }
}
