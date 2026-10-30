// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! A deploy run of 1 project, already on the host: git runs as the git user, hooks as the hooks user.

use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use kukaroll_config::model::{self, ExecutorProject};
use rustix::fs::{FlockOperation, OFlags, flock};

use crate::healthchecks::Healthchecks;
use crate::output::Output;

/// The last successfully deployed commit.
/// A ref, written by git as the git user, so nothing in the git directory ends up owned by anyone else;
/// a fresh clone has none, so it deploys.
pub const STATE_REF: &str = "refs/kukaroll/deployed";

/// How long a failed fetch waits before each retry,
/// so that a forge that is briefly unreachable (a restart, a network blip) doesn't fail the run.
/// Nothing else is retried: the hooks know which of their steps are safe to repeat.
pub const FETCH_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(15), Duration::from_secs(60)];

/// How long 1 fetch attempt may take before it is killed (and retried),
/// so that a forge that accepts the connection but stalls does not hold the run until the unit's timeout.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Run<'a> {
    pub project: &'a str,
    pub config: &'a ExecutorProject,
    pub git: Identity,
    pub hooks: Identity,
    /// Normally `FETCH_RETRY_DELAYS`.
    pub fetch_retry_delays: &'a [Duration],
    /// Normally `FETCH_TIMEOUT`.
    pub fetch_timeout: Duration,
}

/// How commands run as one of the project's users.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    /// Put before each program, e.g. `kukaroll as-user web --`; empty runs it as this process.
    pub prefix: Vec<String>,
    /// The whole environment of the command (see `HostUser::environment`).
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyDeployed(String),
    Deployed(String),
}

impl Run<'_> {
    /// Deploys the branch tip unless it is the last successfully deployed commit.
    /// Reports the outcome to Healthchecks; a failure leaves the deployed commit as it was.
    pub fn execute(&self, output: &mut Output, healthchecks: &Healthchecks) -> Result<Outcome> {
        healthchecks.start(output);
        match self.deploy(output) {
            Ok(outcome) => {
                let summary = match &outcome {
                    Outcome::AlreadyDeployed(commit) => format!("{commit} was already deployed"),
                    Outcome::Deployed(commit) => format!("deployed {commit}"),
                };
                output.say(&summary);
                healthchecks.success(output, summary);
                Ok(outcome)
            }
            Err(error) => {
                output.say(format!("run failed: {error:#}"));
                healthchecks.fail(output);
                Err(error)
            }
        }
    }

    fn deploy(&self, output: &mut Output) -> Result<Outcome> {
        let _lock = self.lock(output)?;
        let config = self.config;
        let tracking_ref = format!("refs/remotes/{}/{}", config.remote, config.branch);
        output.say(format!("fetching {} from {}", config.branch, config.remote));
        self.fetch(output, &tracking_ref)?;
        let tip = self.git_output(&[
            "rev-parse",
            "--verify",
            &format!("{tracking_ref}^{{commit}}"),
        ])?;
        let previous = self.git_output(&["for-each-ref", "--format=%(objectname)", STATE_REF])?;
        if previous == tip {
            return Ok(Outcome::AlreadyDeployed(tip));
        }
        output.say(format!(
            "deploying {tip} (previously deployed: {})",
            if previous.is_empty() {
                "none"
            } else {
                &previous
            }
        ));

        if let Some(command) = &config.pre_flight {
            self.run(output, "pre-flight", self.hook(command, &tip, &previous))?;
        }
        self.run(
            output,
            "git checkout",
            self.git(["checkout", "--force", "--quiet", &tip]),
        )?;
        if let Some(command) = &config.deploy {
            self.run(output, "deploy", self.hook(command, &tip, &previous))?;
        }

        self.git_output(&["update-ref", STATE_REF, &tip])
            .context("cannot record the deploy")?;
        Ok(Outcome::Deployed(tip))
    }

    fn fetch(&self, output: &mut Output, tracking_ref: &str) -> Result<()> {
        let config = self.config;
        let refspec = format!("+refs/heads/{}:{tracking_ref}", config.branch);
        let mut delays = self.fetch_retry_delays.iter();
        loop {
            let command = self.git(["fetch", "--no-tags", &config.remote, &refspec]);
            let result = match output.run_with_timeout(command, self.fetch_timeout) {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => Err(anyhow::anyhow!("git fetch failed ({status})")),
                Err(error) => Err(error.context("git fetch failed")),
            };
            match (result, delays.next()) {
                (Err(error), Some(delay)) => {
                    output.say(format!("{error:#}; retrying in {}s", delay.as_secs_f32()));
                    std::thread::sleep(*delay);
                }
                (result, _) => return result,
            }
        }
    }

    /// Held until the run ends; the kernel releases it if the process dies.
    fn lock(&self, output: &mut Output) -> Result<File> {
        let path = &self.config.lock_file;
        // Read access is all `flock` needs, so a lock file owned by someone else works too.
        // Never through a symlink: whoever can write to its directory must not make root open (or create) another file.
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(OFlags::NOFOLLOW.bits().cast_signed())
            .open(path)
            .with_context(|| format!("cannot open the lock file {}", path.display()))?;
        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
            output.say(format!("waiting for the lock on {}", path.display()));
            flock(&file, FlockOperation::LockExclusive)
                .with_context(|| format!("cannot lock {}", path.display()))?;
        }
        Ok(file)
    }

    fn command(&self, identity: &Identity, program: &str) -> Command {
        let mut command = match identity.prefix.split_first() {
            Some((wrapper, wrapper_args)) => {
                let mut command = Command::new(wrapper);
                command.args(wrapper_args).arg(program);
                command
            }
            None => Command::new(program),
        };
        command
            .current_dir(&self.config.checkout)
            .env_clear()
            .envs(identity.environment.iter().map(|(k, v)| (k, v)));
        command
    }

    fn git<'s>(&self, args: impl IntoIterator<Item = &'s str>) -> Command {
        let mut command = self.command(&self.git, "git");
        // Fail instead of waiting for credentials nobody will type.
        command.env("GIT_TERMINAL_PROMPT", "0").args(args);
        command
    }

    fn hook(&self, command: &model::Command, commit: &str, previous: &str) -> Command {
        let mut hook = self.command(&self.hooks, &command.0[0]);
        hook.args(&command.0[1..])
            // The hooks user may not own the checkout, and git refuses to work in it then.
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "safe.directory")
            .env("GIT_CONFIG_VALUE_0", &self.config.checkout)
            .env("KUKAROLL_PROJECT", self.project)
            .env("KUKAROLL_COMMIT", commit)
            .env("KUKAROLL_PREVIOUS_COMMIT", previous);
        hook
    }

    fn run(&self, output: &mut Output, step: &str, command: Command) -> Result<()> {
        let status = output
            .run(command)
            .with_context(|| format!("{step} failed"))?;
        if !status.success() {
            bail!("{step} failed ({status})");
        }
        Ok(())
    }

    /// Runs a quick git query, returning its trimmed stdout.
    fn git_output(&self, args: &[&str]) -> Result<String> {
        let rendered = format!("git {}", args.join(" "));
        let result = self
            .git(args.iter().copied())
            .output()
            .with_context(|| format!("cannot run {rendered}"))?;
        if !result.status.success() {
            bail!(
                "{rendered} failed ({}): {}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
    }
}
