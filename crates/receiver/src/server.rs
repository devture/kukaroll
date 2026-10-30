// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{self, DefaultBodyLimit, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use kukaroll_config::ReceiverConfig;
use kukaroll_config::model::WebhookProvider;
use tokio::sync::Semaphore;

use crate::adapters::{self, Event, Rejection};

/// Push payloads list the pushed commits, so large pushes make large bodies.
const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Hook requests handled at once: each may buffer up to `BODY_LIMIT` before its signature is checked,
/// so this bounds the memory unauthenticated requests can take. Requests beyond it get a 503 right away.
const MAX_CONCURRENT_HOOKS: usize = 8;

/// Time to receive and handle 1 hook request, so slow senders cannot hold a slot.
const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

struct Project {
    /// `refs/heads/<branch>`, as push payloads name it.
    git_ref: String,
    provider: WebhookProvider,
    secret: Vec<u8>,
}

struct Inner {
    projects: BTreeMap<String, Project>,
    spool_dir: PathBuf,
    hook_slots: Semaphore,
    hook_timeout: Duration,
}

#[derive(Clone)]
pub struct Receiver(Arc<Inner>);

impl Receiver {
    /// Reads every project's secret, so a broken secret file stops the receiver from starting.
    pub fn from_config(config: &ReceiverConfig) -> Result<Self> {
        let mut projects = BTreeMap::new();
        for (name, project) in &config.projects {
            let path = &project.webhook.secret_file;
            let secret = std::fs::read_to_string(path)
                .with_context(|| format!("{name}: cannot read {}", path.display()))?;
            let secret = secret.trim();
            if secret.is_empty() {
                bail!("{name}: {} is empty", path.display());
            }
            projects.insert(
                name.clone(),
                Project {
                    git_ref: format!("refs/heads/{}", project.branch),
                    provider: project.webhook.provider,
                    secret: secret.as_bytes().to_vec(),
                },
            );
        }
        Ok(Self(Arc::new(Inner {
            projects,
            spool_dir: config.spool_dir.clone(),
            hook_slots: Semaphore::new(MAX_CONCURRENT_HOOKS),
            hook_timeout: HOOK_TIMEOUT,
        })))
    }

    /// Replaces the hook request limits (for tests). Call it before the receiver is cloned or serving.
    pub fn with_hook_limits(mut self, max_concurrent: usize, timeout: Duration) -> Self {
        let inner = Arc::get_mut(&mut self.0).expect("the receiver is not shared yet");
        inner.hook_slots = Semaphore::new(max_concurrent);
        inner.hook_timeout = timeout;
        self
    }

    /// Requests a deploy of every project, so pushes missed while the host was down catch up.
    /// Projects already at their branch tip make the executor stop after fetching.
    pub fn catch_up(&self) -> Result<()> {
        for name in self.0.projects.keys() {
            touch(&self.0.spool_dir, name)?;
        }
        Ok(())
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/hooks/{project}",
                post(hook).route_layer(middleware::from_fn_with_state(self.clone(), limit_hooks)),
            )
            .route("/health", get(|| async { "ok" }))
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(self.clone())
    }

    pub async fn serve(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await
            .context("HTTP server failed")
    }
}

/// The executor's `.path` unit fires when the file exists; its content does not matter.
/// An existing file is a request still pending, whoever created it (e.g. root, whose file the receiver cannot open).
/// `create_new` also never follows a symlink.
fn touch(spool_dir: &Path, project: &str) -> Result<()> {
    let path = spool_dir.join(project);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot create {}", path.display())),
    }
}

/// The name comes from the URL: quoted and shortened, so it cannot forge or flood log lines.
fn loggable(name: &str) -> String {
    const MAX_CHARS: usize = 64;
    let shortened: String = name.chars().take(MAX_CHARS).collect();
    let ellipsis = if shortened.len() < name.len() {
        "…"
    } else {
        ""
    };
    format!("{shortened:?}{ellipsis}")
}

/// Runs before the body is read, so requests over the limit never buffer theirs.
async fn limit_hooks(
    extract::State(receiver): extract::State<Receiver>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(_slot) = receiver.0.hook_slots.try_acquire() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "busy\n").into_response();
    };
    match tokio::time::timeout(receiver.0.hook_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, "timed out\n").into_response(),
    }
}

async fn hook(
    extract::State(receiver): extract::State<Receiver>,
    extract::Path(name): extract::Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let Some(project) = receiver.0.projects.get(&name) else {
        eprintln!("{}: 404 unknown project", loggable(&name));
        return (StatusCode::NOT_FOUND, "unknown project\n".into());
    };
    let delivery = adapters::delivery_id(project.provider, &headers).unwrap_or_else(|| "-".into());
    let (status, outcome) =
        match adapters::verify(project.provider, &project.secret, &headers, &body) {
            Err(Rejection::Unauthenticated(reason)) => {
                (StatusCode::UNAUTHORIZED, reason.to_owned())
            }
            Err(Rejection::Malformed(reason)) => (StatusCode::BAD_REQUEST, reason),
            Ok(Event::Ping) => (StatusCode::OK, "pong".into()),
            Ok(Event::Other(event)) => (StatusCode::OK, format!("ignored {event} event")),
            Ok(Event::Push { git_ref, .. }) if git_ref != project.git_ref => {
                (StatusCode::OK, format!("ignored push to {git_ref}"))
            }
            Ok(Event::Push { deleted: true, .. }) => {
                (StatusCode::OK, "ignored deletion of the branch".into())
            }
            Ok(Event::Push { .. }) => match touch(&receiver.0.spool_dir, &name) {
                Ok(()) => (StatusCode::ACCEPTED, "deploy requested".into()),
                Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")),
            },
        };
    eprintln!("{name}: delivery {delivery}: {} {outcome}", status.as_u16());
    if status.is_server_error() {
        // Details (paths) stay in the log.
        return (status, "cannot request a deploy\n".into());
    }
    (status, outcome + "\n")
}
