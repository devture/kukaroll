# SPDX-FileCopyrightText: 2026 Slavi Pantaleev
#
# SPDX-License-Identifier: Apache-2.0
#
# One image for both roles.
# The receiver needs only the binary.
# The executor runs git and the project's hooks inside the host's namespaces, so the image ships nothing else either.

FROM docker.io/rust:1.99.1-alpine3.24@sha256:7e9e2d068a107a2adafd19930011178d14a78326053cd4c3a9f7ad3fd049557d AS build
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
