// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use kukaroll_config::model::{Command, WebhookProvider};
use kukaroll_config::{LoadError, load_executor, load_receiver, parse_executor, parse_receiver};

fn example(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

fn problems(result: Result<impl std::fmt::Debug, LoadError>) -> Vec<String> {
    match result {
        Err(LoadError::Invalid(problems)) => problems.into_iter().map(|p| p.0).collect(),
        other => panic!("expected validation problems, got {other:?}"),
    }
}

fn parse_error(result: Result<impl std::fmt::Debug, LoadError>) -> String {
    match result {
        Err(LoadError::Parse(error)) => error.to_string(),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

const EXECUTOR_PROJECT: &str = "
    checkout: /srv/app
    remote: origin
    branch: main
    git_user: app
    hooks_user: root
    lock_file: /srv/app/deploy.lock
";

fn executor(project_fields: &str) -> String {
    format!("schema_version: 1\nprojects:\n  app:{EXECUTOR_PROJECT}{project_fields}")
}

#[test]
fn examples_are_valid() {
    let receiver = load_receiver(&example("receiver.yaml")).unwrap();
    assert_eq!(
        receiver.projects["myapp"].webhook.provider,
        WebhookProvider::Forgejo
    );

    let executor = load_executor(&example("executor.yaml")).unwrap();
    assert_eq!(
        executor.projects["myapp"].deploy,
        Some(Command(vec!["just".into(), "deploy".into()]))
    );
    let blog = &executor.projects["blog"];
    assert!(blog.pre_flight.is_none() && blog.deploy.is_none() && blog.healthchecks.is_none());
}

#[test]
fn unused_features_must_be_spelled_out_as_null() {
    let error = parse_error(parse_executor(&executor(
        "    deploy: null\n    healthchecks: null\n",
    )));
    assert!(error.contains("missing field `pre_flight`"), "{error}");
}

#[test]
fn unknown_fields_are_rejected() {
    let error = parse_error(parse_executor(&executor(
        "    pre_flight: null\n    deploy: null\n    healthchecks: null\n    timeout: 60\n",
    )));
    assert!(error.contains("unknown field `timeout`"), "{error}");
}

#[test]
fn executor_reports_every_problem() {
    let text = "
schema_version: 2
projects:
  Bad.Name:
    checkout: srv/app
    remote: -origin
    branch: refs/heads/main
    git_user: ''
    hooks_user: 'two words'
    lock_file: deploy.lock
    pre_flight: []
    deploy: ['', 'x']
    healthchecks:
      ping_url: https://hc-ping.com/abc/
";
    let problems = problems(parse_executor(text));
    for expected in [
        "schema_version 2",
        "project name \"Bad.Name\"",
        "projects.Bad.Name.checkout must be an absolute path",
        "projects.Bad.Name.lock_file must be an absolute path",
        "projects.Bad.Name.branch \"refs/heads/main\"",
        "projects.Bad.Name.remote must be a remote name",
        "projects.Bad.Name.git_user must be a user name",
        "projects.Bad.Name.hooks_user must be a user name",
        "projects.Bad.Name.pre_flight must name a program",
        "projects.Bad.Name.deploy must not start with an empty program",
        "projects.Bad.Name.healthchecks.ping_url",
    ] {
        assert!(
            problems.iter().any(|p| p.starts_with(expected)),
            "missing {expected:?} in {problems:#?}"
        );
    }
    assert_eq!(problems.len(), 11, "{problems:#?}");
}

#[test]
fn receiver_reports_every_problem() {
    let text = "
schema_version: 1
listen: 127.0.0.1:8080
spool_dir: /spool
projects:
  app:
    branch: 'main branch'
    webhook:
      provider: gitea
      secret_file: secret
";
    let problems = problems(parse_receiver(text));
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(problems[0].starts_with("projects.app.branch"));
    assert!(problems[1].starts_with("projects.app.webhook.secret_file"));
}

#[test]
fn receiver_rejects_unknown_providers_and_bad_addresses() {
    let base = "schema_version: 1\nspool_dir: /spool\nprojects:\n  app:\n    branch: main\n    webhook:\n      secret_file: /s\n";
    let error = parse_error(parse_receiver(&format!(
        "listen: 127.0.0.1:8080\n{base}      provider: gitlab\n"
    )));
    assert!(error.contains("unknown variant `gitlab`"), "{error}");
    let error = parse_error(parse_receiver(&format!(
        "listen: localhost\n{base}      provider: github\n"
    )));
    assert!(error.contains("listen"), "{error}");
}

#[test]
fn branch_names_follow_git_rules() {
    for (branch, valid) in [
        ("main", true),
        ("release/2026", true),
        ("feature-x_1", true),
        ("-main", false),
        ("a..b", false),
        ("a.lock", false),
        ("a/", false),
        ("a:b", false),
        ("a@{1}", false),
    ] {
        let text = format!(
            "schema_version: 1\nlisten: 127.0.0.1:1\nspool_dir: /s\nprojects:\n  app:\n    branch: '{branch}'\n    webhook: {{provider: github, secret_file: /s}}\n"
        );
        assert_eq!(parse_receiver(&text).is_ok(), valid, "{branch}");
    }
}

#[test]
fn schema_requires_nullable_fields_and_allows_null() {
    let schema =
        serde_json::to_value(schemars::schema_for!(kukaroll_config::ExecutorConfig)).unwrap();
    let project = &schema["$defs"]["ExecutorProject"];
    for field in ["pre_flight", "deploy", "healthchecks"] {
        assert!(
            project["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == field),
            "{field} is not required"
        );
        assert_eq!(
            project["properties"][field]["anyOf"][1]["type"], "null",
            "{field} does not allow null"
        );
    }
}
