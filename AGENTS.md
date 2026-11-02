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
Install the Git pre-commit hook once with `just prek-install-git-pre-commit-hook` (prek, pinned in `mise.toml`); it runs the release-script tests when `release/` or the Dockerfile changes.
CI also runs `just deny` (cargo-deny: advisories, licenses, sources); a new advisory can fail it without any change in the repo.
New source and docs need SPDX/REUSE coverage.
Write documentation for public use with generic examples; keep operator-specific hosts, projects and plans out of the repo.

## Releases

Releases are CalVer Git tags `vYYYY.MM.DD.N` (UTC date; `N` counts from 0 within the day), published as `ghcr.io/devture/kukaroll:vYYYY.MM.DD.N` and `latest`.
Nobody picks versions or pushes tags by hand.
Cargo packages stay at `0.0.0`; release builds embed the real version via `KUKAROLL_VERSION`.

After CI and every 30 minutes, [`release/plan-release.py`](release/plan-release.py) releases main if the image changed since the last release, CI passed and main's head was pushed at least 30 minutes ago;
a Renovate update to the image that may still land holds it back until the head is 90 minutes old.
For a release sooner, start the `Release` workflow by hand (`gh workflow run release.yml`): it skips the waiting, but still needs an image change and passing CI.
The image's paths are the Dockerfile's `COPY` sources plus the `Dockerfile` and `.dockerignore`; keep each `COPY` on 1 line without JSON syntax, or the release scripts refuse it.

[`release/release-notes.py`](release/release-notes.py) writes each GitHub Release's notes from its commits, best effort:
every non-merge commit that touches the image is listed by its subject, so write subjects that read well to people who run kukaroll.
Optional trailers improve the notes; Git only reads trailers in the message's last paragraph,
so put them in 1 block with any `Co-Authored-By:`, without blank lines between them:
`Release-note: <text>` rewords the entry (or includes a commit outside the image, such as documentation),
`Release-note: skip` leaves out a commit nobody upgrading needs to know about,
and `Breaking-change: <what breaks and what to do>` lists the commit first, under "Breaking changes".

## Dependency updates

Renovate bumps the Rust version in `rust-toolchain.toml` and the Dockerfile's `rust` build image in 1 pull request; keep the two equal.
Verify updates with `just check`, and `just image` when the Dockerfile changes.
Never tag or release by hand.

`just e2e` starts a privileged container that acts as root on the machine it runs on.
CI runs it on throwaway runners for every change; run it yourself only when asked to.
