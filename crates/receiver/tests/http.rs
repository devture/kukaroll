// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{KeyInit, Mac};
use kukaroll_receiver::Receiver;
use tower::ServiceExt;

const SECRET: &str = "s3cret";

struct Fixture {
    dir: tempfile::TempDir,
    receiver: Receiver,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("spool")).unwrap();
        std::fs::write(dir.path().join("secret"), format!("  {SECRET}\n")).unwrap();
        let config = kukaroll_config::parse_receiver(&format!(
            "schema_version: 1\nlisten: 127.0.0.1:0\nspool_dir: {root}/spool\nprojects:\n  shop:\n    branch: main\n    webhook: {{provider: forgejo, secret_file: {root}/secret}}\n  blog:\n    branch: pages\n    webhook: {{provider: github, secret_file: {root}/secret}}\n",
            root = dir.path().display()
        ))
        .unwrap();
        let receiver = Receiver::from_config(&config).unwrap();
        Self { dir, receiver }
    }

    fn spooled(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path().join("spool"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    async fn post(&self, path: &str, headers: &[(&str, &str)], body: &str) -> StatusCode {
        let mut request = Request::post(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Body::from(body.to_owned())).unwrap();
        self.receiver
            .router()
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    async fn forgejo_push(&self, project: &str, body: &str) -> StatusCode {
        let signature = sign(body);
        let headers = [
            ("x-forgejo-event", "push"),
            ("x-forgejo-signature", signature.as_str()),
        ];
        self.post(&format!("/hooks/{project}"), &headers, body)
            .await
    }
}

/// Serves on a free local port until the test ends.
async fn serve(receiver: Receiver) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { receiver.serve(listener, std::future::pending()).await });
    address
}

/// Sends a request's head over a new connection (blocking: call it from `spawn_blocking`).
fn send(address: SocketAddr, head_and_body: &str) -> TcpStream {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(head_and_body.as_bytes()).unwrap();
    stream
}

/// A request that announces a body and never sends it.
fn stall(address: SocketAddr) -> TcpStream {
    send(
        address,
        "POST /hooks/shop HTTP/1.1\r\nHost: kukaroll\r\nContent-Length: 10\r\n\r\n",
    )
}

/// An unsigned (so rejected, but handled) request.
fn unsigned(address: SocketAddr) -> TcpStream {
    send(
        address,
        "POST /hooks/shop HTTP/1.1\r\nHost: kukaroll\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    )
}

fn status(mut stream: TcpStream) -> u16 {
    let mut response = [0; 12];
    stream.read_exact(&mut response).unwrap();
    std::str::from_utf8(&response[9..12])
        .unwrap()
        .parse()
        .unwrap()
}

fn sign(body: &str) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[tokio::test]
async fn a_push_to_the_branch_requests_a_deploy() {
    let fixture = Fixture::new();
    let status = fixture
        .forgejo_push("shop", r#"{"ref":"refs/heads/main"}"#)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(fixture.spooled(), ["shop"]);
    // A second push while the first is still pending changes nothing.
    let status = fixture
        .forgejo_push("shop", r#"{"ref":"refs/heads/main"}"#)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(fixture.spooled(), ["shop"]);
}

#[tokio::test]
async fn github_uses_its_own_headers() {
    let fixture = Fixture::new();
    let body = r#"{"ref":"refs/heads/pages","deleted":false}"#;
    let signature = format!("sha256={}", sign(body));
    let headers = [
        ("x-github-event", "push"),
        ("x-hub-signature-256", signature.as_str()),
    ];
    assert_eq!(
        fixture.post("/hooks/blog", &headers, body).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(fixture.spooled(), ["blog"]);
}

#[tokio::test]
async fn other_branches_tags_and_deletions_are_ignored() {
    let fixture = Fixture::new();
    for body in [
        r#"{"ref":"refs/heads/feature"}"#,
        r#"{"ref":"refs/tags/main"}"#,
        r#"{"ref":"refs/heads/main","deleted":true}"#,
    ] {
        assert_eq!(
            fixture.forgejo_push("shop", body).await,
            StatusCode::OK,
            "{body}"
        );
    }
    assert!(fixture.spooled().is_empty());
}

#[tokio::test]
async fn unauthenticated_and_unknown_requests_are_rejected() {
    let fixture = Fixture::new();
    let body = r#"{"ref":"refs/heads/main"}"#;
    let wrong = [("x-forgejo-event", "push"), ("x-forgejo-signature", "00")];
    assert_eq!(
        fixture.post("/hooks/shop", &wrong, body).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .post("/hooks/shop", &[("x-forgejo-event", "push")], body)
            .await,
        StatusCode::UNAUTHORIZED
    );
    // The other project's provider and headers do not apply here.
    assert_eq!(
        fixture
            .forgejo_push("blog", r#"{"ref":"refs/heads/pages"}"#)
            .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture.forgejo_push("nope", body).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture.forgejo_push("../shop", body).await,
        StatusCode::NOT_FOUND
    );
    assert!(fixture.spooled().is_empty());
}

#[tokio::test]
async fn catch_up_requests_every_project() {
    let fixture = Fixture::new();
    fixture.receiver.catch_up().unwrap();
    assert_eq!(fixture.spooled(), ["blog", "shop"]);
}

// E.g. created by root: the receiver cannot open it, but the deploy is requested already.
#[tokio::test]
async fn an_existing_spool_file_counts_as_requested() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let spool_file = fixture.dir.path().join("spool/shop");
    std::fs::write(&spool_file, "").unwrap();
    std::fs::set_permissions(&spool_file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let status = fixture
        .forgejo_push("shop", r#"{"ref":"refs/heads/main"}"#)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    fixture.receiver.catch_up().unwrap();
    assert_eq!(fixture.spooled(), ["blog", "shop"]);
}

#[tokio::test]
async fn spool_failures_are_server_errors() {
    let fixture = Fixture::new();
    std::fs::remove_dir(fixture.dir.path().join("spool")).unwrap();
    let status = fixture
        .forgejo_push("shop", r#"{"ref":"refs/heads/main"}"#)
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!Path::new(&fixture.dir.path().join("spool")).exists());
}

#[tokio::test]
async fn slow_requests_time_out() {
    let fixture = Fixture::new();
    let address = serve(
        fixture
            .receiver
            .with_hook_limits(8, Duration::from_millis(200)),
    )
    .await;
    let status = tokio::task::spawn_blocking(move || status(stall(address)))
        .await
        .unwrap();
    assert_eq!(status, 408);
    let mut spool = std::fs::read_dir(fixture.dir.path().join("spool")).unwrap();
    assert!(spool.next().is_none());
}

#[tokio::test]
async fn requests_beyond_the_limit_are_turned_away_until_a_slot_frees() {
    let fixture = Fixture::new();
    let address = serve(
        fixture
            .receiver
            .with_hook_limits(1, Duration::from_millis(500)),
    )
    .await;
    let statuses = tokio::task::spawn_blocking(move || {
        let held = stall(address);
        std::thread::sleep(Duration::from_millis(100));
        let turned_away = status(unsigned(address));
        let held = status(held);
        (turned_away, held, status(unsigned(address)))
    })
    .await
    .unwrap();
    assert_eq!(statuses, (503, 408, 401));
}

#[test]
fn empty_secrets_stop_the_receiver() {
    let fixture = Fixture::new();
    std::fs::write(fixture.dir.path().join("secret"), "\n").unwrap();
    let config = kukaroll_config::parse_receiver(&format!(
        "schema_version: 1\nlisten: 127.0.0.1:0\nspool_dir: /spool\nprojects:\n  shop:\n    branch: main\n    webhook: {{provider: gitea, secret_file: {}/secret}}\n",
        fixture.dir.path().display()
    ))
    .unwrap();
    let error = Receiver::from_config(&config).err().unwrap();
    assert!(format!("{error:#}").contains("is empty"), "{error:#}");
}
