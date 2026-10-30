// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Configuration of the receiver and the executor: 2 YAML files,
//! so the internet-facing receiver never sees executor settings.
//! Nothing that changes behavior has an implicit default; unused features are spelled out as `null`.
//! Loading stops at the first syntax or structure error (an unknown or missing field, a wrong type);
//! otherwise it returns every invalid value, not just the first.

pub mod model;
pub mod validate;

use std::path::Path;

use serde::de::DeserializeOwned;

pub use model::{ExecutorConfig, ReceiverConfig};
pub use validate::{Problem, validate_executor, validate_receiver};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    // The inner errors are part of the message, not sources, so they print once in error chains.
    #[error("cannot read {path}: {error}")]
    Io { path: String, error: std::io::Error },
    #[error("cannot parse configuration: {0}")]
    Parse(serde_saphyr::Error),
    #[error("configuration is invalid:\n{}", .0.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<Problem>),
}

pub fn load_receiver(path: &Path) -> Result<ReceiverConfig, LoadError> {
    parse_receiver(&read(path)?)
}

pub fn load_executor(path: &Path) -> Result<ExecutorConfig, LoadError> {
    parse_executor(&read(path)?)
}

pub fn parse_receiver(text: &str) -> Result<ReceiverConfig, LoadError> {
    parse(text, validate_receiver)
}

pub fn parse_executor(text: &str) -> Result<ExecutorConfig, LoadError> {
    parse(text, validate_executor)
}

fn read(path: &Path) -> Result<String, LoadError> {
    std::fs::read_to_string(path).map_err(|error| LoadError::Io {
        path: path.display().to_string(),
        error,
    })
}

fn parse<T: DeserializeOwned>(
    text: &str,
    validate: fn(&T) -> Vec<Problem>,
) -> Result<T, LoadError> {
    // Tags that mean nothing to kukaroll are mistakes, not something to ignore.
    let mut options = serde_saphyr::Options::default();
    options.reject_unsupported_tags = true;
    let config: T = serde_saphyr::from_str_with_options(text, options).map_err(LoadError::Parse)?;
    let problems = validate(&config);
    if problems.is_empty() {
        Ok(config)
    } else {
        Err(LoadError::Invalid(problems))
    }
}
