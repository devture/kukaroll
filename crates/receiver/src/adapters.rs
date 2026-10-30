// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Forge webhook adapters: verify a delivery's signature, then say what kind of event it is.
//!
//! All supported forges sign the raw body with HMAC-SHA256 and describe pushes with the same `ref`/`deleted` fields;
//! they differ only in header names.

use axum::http::HeaderMap;
use hmac::{KeyInit, Mac};
use kukaroll_config::model::WebhookProvider;
use serde::Deserialize;
use sha2::Sha256;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Push {
        git_ref: String,
        deleted: bool,
    },
    /// Sent when a webhook is created (GitHub) or tested.
    Ping,
    Other(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Missing or wrong signature: the request is not from the forge.
    Unauthenticated(&'static str),
    /// Correctly signed but not understood.
    Malformed(String),
}

struct Headers {
    signature: &'static str,
    signature_prefix: &'static str,
    event: &'static str,
    delivery: &'static str,
}

fn headers(provider: WebhookProvider) -> Headers {
    match provider {
        WebhookProvider::Forgejo => Headers {
            signature: "x-forgejo-signature",
            signature_prefix: "",
            event: "x-forgejo-event",
            delivery: "x-forgejo-delivery",
        },
        WebhookProvider::Gitea => Headers {
            signature: "x-gitea-signature",
            signature_prefix: "",
            event: "x-gitea-event",
            delivery: "x-gitea-delivery",
        },
        WebhookProvider::Github => Headers {
            signature: "x-hub-signature-256",
            signature_prefix: "sha256=",
            event: "x-github-event",
            delivery: "x-github-delivery",
        },
    }
}

/// The forge's delivery ID, for logs only (unauthenticated until `verify` succeeds).
pub fn delivery_id(provider: WebhookProvider, request_headers: &HeaderMap) -> Option<String> {
    header(request_headers, headers(provider).delivery).map(str::to_owned)
}

pub fn verify(
    provider: WebhookProvider,
    secret: &[u8],
    request_headers: &HeaderMap,
    body: &[u8],
) -> Result<Event, Rejection> {
    let names = headers(provider);
    let signature = header(request_headers, names.signature)
        .ok_or(Rejection::Unauthenticated("missing signature"))?;
    let signature = signature
        .strip_prefix(names.signature_prefix)
        .and_then(|hex_digest| hex::decode(hex_digest).ok())
        .ok_or(Rejection::Unauthenticated("malformed signature"))?;
    let mut mac =
        hmac::Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(body);
    mac.verify_slice(&signature)
        .map_err(|_| Rejection::Unauthenticated("wrong signature"))?;

    let event = header(request_headers, names.event)
        .ok_or_else(|| Rejection::Malformed(format!("missing {} header", names.event)))?;
    match event {
        "push" => {
            #[derive(Deserialize)]
            struct Push {
                #[serde(rename = "ref")]
                git_ref: String,
                #[serde(default)]
                deleted: bool,
            }
            let push: Push = serde_json::from_slice(body)
                .map_err(|error| Rejection::Malformed(format!("push payload: {error}")))?;
            Ok(Event::Push {
                git_ref: push.git_ref,
                deleted: push.deleted,
            })
        }
        "ping" => Ok(Event::Ping),
        other => Ok(Event::Other(other.to_owned())),
    }
}

fn header<'a>(request_headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    request_headers.get(name)?.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"s3cret";
    const BODY: &[u8] = br#"{"ref":"refs/heads/main","before":"a","after":"b","deleted":false}"#;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn request(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    fn push(git_ref: &str, deleted: bool) -> Result<Event, Rejection> {
        Ok(Event::Push {
            git_ref: git_ref.into(),
            deleted,
        })
    }

    #[test]
    fn forgejo_gitea_and_github_pushes_verify() {
        let signature = sign(SECRET, BODY);
        let cases = [
            (
                WebhookProvider::Forgejo,
                "x-forgejo-signature",
                "x-forgejo-event",
                signature.clone(),
            ),
            (
                WebhookProvider::Gitea,
                "x-gitea-signature",
                "x-gitea-event",
                signature.clone(),
            ),
            (
                WebhookProvider::Github,
                "x-hub-signature-256",
                "x-github-event",
                format!("sha256={signature}"),
            ),
        ];
        for (provider, signature_header, event_header, value) in cases {
            let headers = request(&[(signature_header, value), (event_header, "push".into())]);
            assert_eq!(
                verify(provider, SECRET, &headers, BODY),
                push("refs/heads/main", false)
            );
        }
    }

    #[test]
    fn signatures_are_checked_against_secret_and_body() {
        let headers = |signature: String| {
            request(&[
                ("x-gitea-signature", signature),
                ("x-gitea-event", "push".into()),
            ])
        };
        let unauthenticated = |headers: &HeaderMap, body: &[u8]| {
            matches!(
                verify(WebhookProvider::Gitea, SECRET, headers, body),
                Err(Rejection::Unauthenticated(_))
            )
        };
        assert!(unauthenticated(&headers(sign(b"other", BODY)), BODY));
        assert!(unauthenticated(&headers(sign(SECRET, BODY)), b"{}"));
        assert!(unauthenticated(&headers("zz".into()), BODY));
        assert!(unauthenticated(
            &request(&[("x-gitea-event", "push".into())]),
            BODY
        ));
        // GitHub's prefix is required, and other forges' headers do not count for GitHub.
        let github = request(&[
            ("x-hub-signature-256", sign(SECRET, BODY)),
            ("x-gitea-signature", sign(SECRET, BODY)),
            ("x-github-event", "push".into()),
        ]);
        assert!(matches!(
            verify(WebhookProvider::Github, SECRET, &github, BODY),
            Err(Rejection::Unauthenticated(_))
        ));
    }

    #[test]
    fn other_events_and_payloads() {
        let signed = |event: &str, body: &[u8]| {
            let headers = request(&[
                ("x-forgejo-signature", sign(SECRET, body)),
                ("x-forgejo-event", event.into()),
            ]);
            verify(WebhookProvider::Forgejo, SECRET, &headers, body)
        };
        assert_eq!(signed("ping", b"{}"), Ok(Event::Ping));
        assert_eq!(signed("issues", b"{}"), Ok(Event::Other("issues".into())));
        assert_eq!(
            signed("push", br#"{"ref":"refs/heads/x","deleted":true}"#),
            push("refs/heads/x", true)
        );
        assert_eq!(
            signed("push", br#"{"ref":"refs/tags/v1"}"#),
            push("refs/tags/v1", false)
        );
        assert!(matches!(
            signed("push", b"{}"),
            Err(Rejection::Malformed(_))
        ));
        assert!(matches!(
            signed("push", b"not json"),
            Err(Rejection::Malformed(_))
        ));
    }
}
