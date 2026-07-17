# syntax=docker/dockerfile:1.7

FROM rust:1.85-alpine3.21 AS build

ARG TARGETARCH
ARG CARGO_PROFILE=dev
WORKDIR /build
RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
# Keep dependency compilation in a manifest-only layer. Source-only edits reuse
# this layer, while the cache mounts retain Cargo downloads and target outputs.
RUN mkdir src && printf 'fn main() {}\n' > src/main.rs
RUN --mount=type=cache,id=harness-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=harness-target-${TARGETARCH},target=/build/target \
    cargo build --locked --profile "${CARGO_PROFILE}"

COPY src ./src
RUN --mount=type=cache,id=harness-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=harness-target-${TARGETARCH},target=/build/target \
    touch src/main.rs && \
    cargo build --locked --profile "${CARGO_PROFILE}" && \
    mkdir /out && \
    case "${CARGO_PROFILE}" in \
        dev) artifact_dir=debug ;; \
        *) artifact_dir="${CARGO_PROFILE}" ;; \
    esac && \
    cp "target/${artifact_dir}/harness" /out/harness

FROM scratch AS artifact
COPY --from=build /out/harness /harness
