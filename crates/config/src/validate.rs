// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use crate::model::{Command, ExecutorConfig, ReceiverConfig, SCHEMA_VERSION};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Problem(pub String);

/// Project names end up in URL paths, spool file names and systemd instance names.
pub fn is_project_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A conservative subset of `git check-ref-format --branch`: a plain branch name, not a full ref.
fn is_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.starts_with("refs/")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
}

/// Remote and user names: non-empty, no whitespace or separators, not option-like.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && !name
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace() || "/:\\".contains(c))
}

struct Problems(Vec<Problem>);

impl Problems {
    fn push(&mut self, message: impl Into<String>) {
        self.0.push(Problem(message.into()));
    }

    fn schema_version(&mut self, version: u32) {
        if version != SCHEMA_VERSION {
            self.push(format!(
                "schema_version {version} is not the supported {SCHEMA_VERSION}"
            ));
        }
    }

    fn absolute(&mut self, field: &str, path: &Path) {
        if !path.is_absolute() {
            self.push(format!("{field} must be an absolute path"));
        }
    }

    fn project_name(&mut self, name: &str) {
        if !is_project_name(name) {
            self.push(format!(
                "project name {name:?} must be 1-64 characters of a-z, 0-9, '-' and '_', starting with a letter or digit"
            ));
        }
    }

    fn branch(&mut self, field: &str, branch: &str) {
        if !is_branch_name(branch) {
            self.push(format!(
                "{field} {branch:?} must be a plain branch name (like \"main\", not \"refs/heads/main\")"
            ));
        }
    }

    fn command(&mut self, field: &str, command: &Command) {
        match command.0.first() {
            None => self.push(format!(
                "{field} must name a program (use null to skip the step)"
            )),
            Some(program) if program.is_empty() => {
                self.push(format!("{field} must not start with an empty program"));
            }
            Some(_) => {}
        }
        if command.0.iter().any(|argument| argument.contains('\0')) {
            self.push(format!("{field} must not contain NUL characters"));
        }
    }
}

pub fn validate_receiver(config: &ReceiverConfig) -> Vec<Problem> {
    let mut problems = Problems(Vec::new());
    problems.schema_version(config.schema_version);
    problems.absolute("spool_dir", &config.spool_dir);
    for (name, project) in &config.projects {
        problems.project_name(name);
        problems.branch(&format!("projects.{name}.branch"), &project.branch);
        problems.absolute(
            &format!("projects.{name}.webhook.secret_file"),
            &project.webhook.secret_file,
        );
    }
    problems.0
}

pub fn validate_executor(config: &ExecutorConfig) -> Vec<Problem> {
    let mut problems = Problems(Vec::new());
    problems.schema_version(config.schema_version);
    for (name, project) in &config.projects {
        let field = |suffix: &str| format!("projects.{name}.{suffix}");
        problems.project_name(name);
        problems.absolute(&field("checkout"), &project.checkout);
        problems.absolute(&field("lock_file"), &project.lock_file);
        problems.branch(&field("branch"), &project.branch);
        if !is_plain_name(&project.remote) {
            problems.push(format!("{} must be a remote name", field("remote")));
        }
        for (key, user) in [
            ("git_user", &project.git_user),
            ("hooks_user", &project.hooks_user),
        ] {
            if !is_plain_name(user) {
                problems.push(format!("{} must be a user name", field(key)));
            }
        }
        if let Some(command) = &project.pre_flight {
            problems.command(&field("pre_flight"), command);
        }
        if let Some(command) = &project.deploy {
            problems.command(&field("deploy"), command);
        }
        if let Some(healthchecks) = &project.healthchecks {
            let url = &healthchecks.ping_url;
            let scheme_ok = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .is_some_and(|rest| !rest.is_empty());
            if !scheme_ok
                || url.ends_with('/')
                || url.contains(['?', '#'])
                || url.contains(char::is_whitespace)
            {
                problems.push(format!(
                    "{} must be an http(s) URL without a trailing slash, query or fragment",
                    field("healthchecks.ping_url")
                ));
            }
        }
    }
    problems.0
}
