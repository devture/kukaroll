# SPDX-FileCopyrightText: 2026 Slavi Pantaleev
#
# SPDX-License-Identifier: Apache-2.0

# Show help by default
default:
	@just --list --justfile {{ justfile() }}

# Builds a development binary (target/debug/kukaroll)
build-dev *extra_args:
	RUST_BACKTRACE=1 cargo build --workspace {{ extra_args }}

# Builds an optimized release binary (target/release/kukaroll)
build-release *extra_args: (build-dev "--release")

# Runs the clippy linter
clippy *extra_args:
	cargo clippy --workspace --all-targets {{ extra_args }} -- -D warnings

# Checks formatting
fmt-check:
	cargo fmt --all -- --check

# Runs tests
test *extra_args:
	cargo test --workspace {{ extra_args }}

# Tests the release planning scripts
release-test:
	python3 -m unittest discover -s release -p 'test_*.py'

# Runs the REUSE compliance check
reuse-lint:
	reuse lint

# Checks dependencies for advisories, licenses and sources (needs cargo-deny and network access; CI runs it)
deny:
	cargo deny --locked check

# Builds the container image
image tag="kukaroll:dev":
	docker build --tag {{ tag }} .

# Runs everything CI would run (except the image build)
check: fmt-check clippy test release-test reuse-lint

# End-to-end test of the image (runs a privileged container that acts on this host; see the script)
e2e tag="kukaroll:dev": (image tag)
	bin/e2e.sh {{ tag }}
