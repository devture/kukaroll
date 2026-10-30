// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use kukaroll_config::{ExecutorConfig, ReceiverConfig};
use kukaroll_executor::host::HostUser;

/// Release builds receive their CalVer version at compile time; Cargo cannot represent it.
const VERSION: &str = match option_env!("KUKAROLL_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => "dev",
};

#[derive(Parser)]
#[command(name = "kukaroll", version = VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serves forge webhooks and touches the spool file of each project that should be deployed.
    Receive {
        #[arg(long)]
        config: PathBuf,
    },
    /// Deploys the tip of one project's branch (run by its systemd unit, inside the host's namespaces).
    Execute {
        #[arg(long)]
        config: PathBuf,
        project: String,
    },
    /// Validates a configuration file: stops at the first syntax or structure error, otherwise reports every invalid value.
    CheckConfig { kind: ConfigKind, path: PathBuf },
    /// Prints the JSON Schema of a configuration file.
    Schema { kind: ConfigKind },
    /// Switches to a host user and execs the command, keeping the environment and working directory
    /// (used by the executor, already in the host's namespaces, for git and the hooks).
    #[command(hide = true)]
    AsUser {
        user: String,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Runs a command on the host as the given user (for testing).
    #[command(hide = true)]
    HostExec {
        user: String,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfigKind {
    Receiver,
    Executor,
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Receive { config } => {
            let config = kukaroll_config::load_receiver(&config)?;
            let receiver = kukaroll_receiver::Receiver::from_config(&config)?;
            receiver.catch_up()?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let listener = tokio::net::TcpListener::bind(config.listen)
                        .await
                        .with_context(|| format!("cannot listen on {}", config.listen))?;
                    eprintln!(
                        "kukaroll {VERSION} receiving webhooks for {} project(s) on {}",
                        config.projects.len(),
                        config.listen
                    );
                    receiver.serve(listener, shutdown_signal()).await
                })
        }
        Command::Execute { config, project } => {
            let config = kukaroll_config::load_executor(&config)?;
            kukaroll_executor::execute(&config, &project)
        }
        Command::CheckConfig { kind, path } => {
            match kind {
                ConfigKind::Receiver => drop(kukaroll_config::load_receiver(&path)?),
                ConfigKind::Executor => drop(kukaroll_config::load_executor(&path)?),
            }
            println!("{} is valid", path.display());
            Ok(())
        }
        Command::Schema { kind } => {
            let schema = match kind {
                ConfigKind::Receiver => schemars::schema_for!(ReceiverConfig),
                ConfigKind::Executor => schemars::schema_for!(ExecutorConfig),
            };
            println!("{}", serde_json::to_string_pretty(&schema)?);
            Ok(())
        }
        Command::AsUser { user, command } => {
            HostUser::lookup(&user)?.become_this_user()?;
            let error = std::process::Command::new(&command[0])
                .args(&command[1..])
                .exec();
            Err(error).with_context(|| format!("cannot run {}", command[0]))
        }
        Command::HostExec { user, command } => {
            kukaroll_executor::host::enter_host_namespaces()?;
            let user = HostUser::lookup(&user)?;
            user.become_this_user()?;
            let status = std::process::Command::new(&command[0])
                .args(&command[1..])
                .env_clear()
                .envs(user.environment())
                .current_dir(&user.home)
                .status()?;
            anyhow::ensure!(status.success(), "{} exited with {status}", command[0]);
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("cannot listen for SIGTERM");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<kukaroll_executor::Reported>() => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("kukaroll: {error:#}");
            ExitCode::FAILURE
        }
    }
}
