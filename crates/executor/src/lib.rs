// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Per-project executor: lock, fetch, pre-flight, checkout, deploy hook, Healthchecks.

pub mod healthchecks;
pub mod host;
pub mod output;
pub mod run;

use anyhow::{Context, Result};
use kukaroll_config::ExecutorConfig;

use crate::healthchecks::Healthchecks;
use crate::host::HostUser;
use crate::output::Output;
use crate::run::{FETCH_RETRY_DELAYS, FETCH_TIMEOUT, Identity, Run};

/// The error of a run that has already logged its failure (`run failed: …`) and reported it to Healthchecks.
#[derive(Debug)]
pub struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the run failed")
    }
}

impl std::error::Error for Reported {}

/// Deploys 1 project from inside a privileged container sharing the host's PID namespace.
/// Must be called while the process is still single-threaded.
/// Fails with [`Reported`] once a run has started; errors before that are not logged yet.
pub fn execute(config: &ExecutorConfig, project: &str) -> Result<()> {
    let project_config = config
        .projects
        .get(project)
        .with_context(|| format!("project {project:?} is not configured"))?;
    // The container has no network of its own, so nothing can be reported before this.
    host::enter_host_namespaces()?;

    let mut output = Output::new();
    let healthchecks = Healthchecks::new(
        project_config
            .healthchecks
            .as_ref()
            .map(|healthchecks| healthchecks.ping_url.clone()),
    );

    // The executor stays root; each git or hook command switches users on its own.
    let users = HostUser::lookup(&project_config.git_user)
        .and_then(|git| Ok((git, HostUser::lookup(&project_config.hooks_user)?)));
    let (git, hooks) = match users {
        Ok((git, hooks)) => (identity(&git), identity(&hooks)),
        Err(error) => {
            healthchecks.start(&mut output);
            output.say(format!("run failed: {error:#}"));
            healthchecks.fail(&mut output);
            return Err(Reported.into());
        }
    };

    Run {
        project,
        config: project_config,
        git,
        hooks,
        fetch_retry_delays: &FETCH_RETRY_DELAYS,
        fetch_timeout: FETCH_TIMEOUT,
    }
    .execute(&mut output, &healthchecks)
    .map_err(|_| Reported)?;
    Ok(())
}

/// Commands run through this binary's hidden `as-user` subcommand, which drops to the user and execs the program.
/// A fresh process is single-threaded, so dropping credentials there is safe,
/// and `/proc/self/exe` works from the host's mount namespace, where the binary's path does not.
fn identity(user: &HostUser) -> Identity {
    Identity {
        prefix: vec![
            "/proc/self/exe".into(),
            "as-user".into(),
            user.name.clone(),
            "--".into(),
        ],
        environment: user.environment(),
    }
}
