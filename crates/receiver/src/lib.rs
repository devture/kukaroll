// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Webhook receiver: verifies forge webhooks and touches a project's spool file.
//!
//! It passes nothing from a request on: the executor deploys whatever the branch tip is when it fetches,
//! so a compromised receiver can at most start runs that deploy the branch's current tip.

pub mod adapters;
mod server;

pub use server::Receiver;
