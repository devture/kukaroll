<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Working on kukaroll

kukaroll is a small Rust deployer: a webhook receiver plus a per-project executor run by systemd.
Read the [architecture](docs/architecture.md) first.

## Working rules

Keep the tool git-aware and project-agnostic: it never learns about Compose, databases, `.env` files or any project's conventions; those belong in each project's hooks.
Configuration has no implicit defaults: every behavior-changing field is required (`null` where a feature is unused).

The receiver is internet-facing and must stay unprivileged: it only touches spool files and never passes data from a request to the executor.
The executor trusts nothing from the network.

Use `just` for build and check entry points; `just check` covers formatting, Clippy, tests, the release scripts and REUSE.
CI also runs `just deny` (cargo-deny: advisories, licenses, sources); a new advisory can fail it without any change in the repo.
New source and docs need SPDX/REUSE coverage.
Write documentation for public use with generic examples; keep operator-specific hosts, projects and plans out of the repo.

## Releases

Releases are CalVer Git tags `vYYYY.MM.DD.N` (UTC date; `N` counts from 0 within the day), published as `ghcr.io/devture/kukaroll:YYYY.MM.DD.N` and `latest`.
Nobody picks versions or pushes tags by hand.
Cargo packages stay at `0.0.0`; release builds embed the real version via `KUKAROLL_VERSION`.

`.github/workflows/release.yml` runs after every CI run on main, and every 30 minutes as a fallback.
[`release/plan-release.py`](release/plan-release.py) releases main's head commit when something in the image changed since the last tag (`IMAGE_PATHS`) and CI passed,
and either no open Renovate PR may still land or the commit is at least 90 minutes old.
A manual `Release` run releases immediately if the image changed.

## Dependency updates

Renovate bumps the Rust version in `rust-toolchain.toml` and the Dockerfile's `rust` build image in 1 pull request; keep the two equal.
Verify updates with `just check`, and `just image` when the Dockerfile changes.
Never tag or release by hand.

`just e2e` starts a privileged container that acts as root on the machine it runs on.
CI runs it on throwaway runners for every change; run it yourself only when asked to.
