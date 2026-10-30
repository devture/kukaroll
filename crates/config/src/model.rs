// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Configuration of the internet-facing receiver. It never contains executor settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiverConfig {
    pub schema_version: u32,
    /// Address the HTTP server listens on, e.g. `0.0.0.0:8080`.
    pub listen: SocketAddr,
    /// Directory holding 1 spool file per project, as seen by the receiver.
    pub spool_dir: PathBuf,
    /// Keyed by project name. Webhooks for a project arrive at `/hooks/<project>`.
    pub projects: BTreeMap<String, ReceiverProject>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiverProject {
    /// Branch whose pushes trigger a deploy (a name like `main`, not `refs/heads/main`).
    pub branch: String,
    pub webhook: Webhook,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    pub provider: WebhookProvider,
    /// File holding the webhook secret (surrounding whitespace is ignored).
    pub secret_file: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookProvider {
    /// HMAC-SHA256 in `X-Forgejo-Signature`.
    Forgejo,
    /// HMAC-SHA256 in `X-Gitea-Signature` (Forgejo sends it too).
    Gitea,
    /// HMAC-SHA256 in `X-Hub-Signature-256`.
    Github,
}

/// Configuration of the per-project executor, read before it enters the host's namespaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutorConfig {
    pub schema_version: u32,
    /// Keyed by project name; must match the receiver's.
    pub projects: BTreeMap<String, ExecutorProject>,
}

/// Paths are host paths: the executor works inside the host's namespaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutorProject {
    /// The git working copy that gets deployed.
    pub checkout: PathBuf,
    /// Name of the remote (as configured in the checkout) to fetch from.
    pub remote: String,
    /// Branch to deploy (a name like `main`, not `refs/heads/main`).
    pub branch: String,
    /// Host user that runs git (home directory, groups, SSH keys); usually the checkout's owner.
    pub git_user: String,
    /// Host user that runs the hooks; may differ from `git_user`, e.g. `root` for `docker compose`.
    pub hooks_user: String,
    /// File locked with `flock` for the whole run; other tools may lock it to keep deploys out.
    pub lock_file: PathBuf,
    /// Runs in the checkout before `git checkout -f`, so against the old commit. `null` skips it.
    #[serde(deserialize_with = "nullable")]
    #[schemars(with = "Nullable<Command>")]
    pub pre_flight: Option<Command>,
    /// Runs in the checkout after `git checkout -f`, so against the new commit. `null` skips it.
    #[serde(deserialize_with = "nullable")]
    #[schemars(with = "Nullable<Command>")]
    pub deploy: Option<Command>,
    /// `null` disables reporting.
    #[serde(deserialize_with = "nullable")]
    #[schemars(with = "Nullable<Healthchecks>")]
    pub healthchecks: Option<Healthchecks>,
}

/// A program and its arguments, run without a shell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Command(pub Vec<String>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Healthchecks {
    /// The check's full ping URL; `/start` and `/fail` are appended to it.
    pub ping_url: String,
}

/// With `deserialize_with`, serde stops treating a missing `Option` field as `None`, so the field must be present.
/// That is the point: unused features are spelled out as `null`.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Schema of a field that must be present but may be `null`.
/// schemars treats `Option` fields as optional, and marking them required drops `null` from their schema.
struct Nullable<T>(PhantomData<T>);

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({"anyOf": [generator.subschema_for::<T>(), {"type": "null"}]})
    }
}
