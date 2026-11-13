# SPDX-FileCopyrightText: 2026 Slavi Pantaleev
#
# SPDX-License-Identifier: Apache-2.0
#
# One image for both roles.
# The receiver needs only the binary.
# The executor runs git and the project's hooks inside the host's namespaces, so the image ships nothing else either.

FROM docker.io/rust:1.100.0-alpine3.24@sha256:de1e0058543a81ed7b046629167db587c67a94bdf37cce14eb27f0ec83baaffb AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY crates/ ./crates/
ARG KUKAROLL_VERSION=dev
RUN cargo build --release --locked --bin kukaroll \
    && readelf --program-headers --dynamic target/release/kukaroll > /tmp/kukaroll-elf \
    && ! grep -Eq 'INTERP|\(NEEDED\)' /tmp/kukaroll-elf \
    && target/release/kukaroll --version

FROM scratch
LABEL org.opencontainers.image.source="https://github.com/devture/kukaroll" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /build/target/release/kukaroll /kukaroll
COPY LICENSES/Apache-2.0.txt /LICENSE.txt
# The receiver runs unprivileged by default; the executor's unit passes --user 0:0 explicitly.
USER 65534:65534
ENTRYPOINT ["/kukaroll"]
