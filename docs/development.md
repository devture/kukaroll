<!--
SPDX-FileCopyrightText: 2026 Slavi Pantaleev

SPDX-License-Identifier: Apache-2.0
-->

# Development

kukaroll is a Rust workspace; [`just`](https://just.systems/) runs every build and check.
Read the [architecture](architecture.md) first.

## Checks

- `just check` runs formatting, Clippy, the tests, the release scripts' tests and REUSE.
- `just deny` checks the dependencies with [cargo-deny](https://embarkstudios.github.io/cargo-deny/): RustSec advisories, licenses and sources (see `deny.toml`).
  A new advisory can fail it without any change in the repository.
- `just image` builds the container image.

CI runs all of them for every change.

## The end-to-end test

`just e2e` builds the image and runs a push through the receiver and the executor against a throwaway repository, with git as the current user and the hooks as root, in a temporary directory.
It also checks that a run cut short (as on the unit's timeout) takes every hook process with it.

It starts a privileged container that acts as root on the machine it runs on.
CI runs it for both architectures on throwaway runners.

## Releases

Releases are automatic: CI tags main's head commit when the image changed and the checks passed, and publishes the image.
See [Installing](installing.md#container-image) for the version scheme.
