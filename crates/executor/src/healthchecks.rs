// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Optional reporting to Healthchecks.
//! A failing ping is logged and never fails the deploy.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

use crate::output::Output;

pub struct Healthchecks {
    ping_url: Option<String>,
    agent: ureq::Agent,
}

impl Healthchecks {
    pub fn new(ping_url: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            // The executor runs in the host's namespaces, so these are the host's CA certificates.
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        Self { ping_url, agent }
    }

    pub fn start(&self, output: &mut Output) {
        self.ping(output, "/start", String::new());
    }

    pub fn success(&self, output: &mut Output, summary: String) {
        self.ping(output, "", summary);
    }

    pub fn fail(&self, output: &mut Output) {
        let tail = output.tail();
        self.ping(output, "/fail", tail);
    }

    fn ping(&self, output: &mut Output, suffix: &str, body: String) {
        let Some(base) = &self.ping_url else {
            return;
        };
        let url = format!("{base}{suffix}");
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.agent.post(&url).send(body.as_str()) {
                Ok(_) => return,
                Err(error) => last_error = Some(error),
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        if let Some(error) = last_error {
            // Never print the URL: it identifies (and lets anyone ping) the check.
            output.say(format!(
                "Healthchecks ping ({}) failed: {error}",
                if suffix.is_empty() {
                    "success"
                } else {
                    &suffix[1..]
                }
            ));
        }
    }
}
