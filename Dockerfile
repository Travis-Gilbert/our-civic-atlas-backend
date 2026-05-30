# Dockerfile for our-civic-atlas-backend.
#
# Built explicitly because Railpack's auto-detected Rust runtime image
# does not include /app/target/release/ — only the binary it can
# uniquely identify, and a Cargo workspace with multiple binaries
# leaves it ambiguous. Two-stage build keeps the runtime image small.
#
# Build:   docker build -t civic-atlas-backend .
# Run:     docker run -p 8080:8080 -e PORT=8080 civic-atlas-backend
#
# Deploy note (2026-05-30): Railway deploys this repo's `main` branch and the
# civic-atlas-server build requires rustc >= 1.89 (async-graphql 7.2.1,
# asynk-strim 0.1.5). Do not redeploy pre-1.89 commits such as the 8bdf250-era
# seed commits; they fail the build on the old 1.88 pin. Always deploy main's
# current tip.

# --- stage 1: build ---------------------------------------------------
# rust:1.89 because async-graphql@7.2.1 (pulled in by the Axum-native
# GraphQL surface in crates/civic-atlas-server/src/graphql) requires
# rustc >= 1.89. Earlier transitive deps (time, home, icu_*) needed
# 1.86/1.88; async-graphql is the current floor. Workspace's
# Cargo.toml rust-version declaration is a min-version hint for
# downstream consumers, not a deps constraint.
FROM rust:1.89-slim-bookworm AS builder

# protoc is needed by tonic-build to compile civic_atlas.proto and
# friends at build time. The civic-atlas-types build.rs invokes it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        protobuf-compiler \
        pkg-config \
        libssl-dev \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the whole workspace. We're optimizing for build correctness
# over incremental rebuild cache hits; the prod image is a one-shot.
COPY . .

# Build only the API server. The CLI and outbox worker live in the
# same workspace but ship as separate Railway services (each can
# point at this same repo with its own startCommand).
RUN cargo build --release --bin civic-atlas-server


# --- stage 2: runtime -------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Minimum runtime deps. ca-certificates for outbound HTTPS (e.g.
# Theseus bridge). libssl3 covers rustls fallback paths some sqlx
# features hit.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy only the release binary out of the builder stage.
COPY --from=builder /build/target/release/civic-atlas-server /app/civic-atlas-server

# Document the port. Railway injects PORT at runtime; main.rs binds
# to 0.0.0.0:$PORT via the env-var fallback added in 024241d.
EXPOSE 8080

# Use exec form so the process gets PID 1 and shuts down cleanly on
# SIGTERM (Railway uses SIGTERM for graceful shutdown).
ENTRYPOINT ["/app/civic-atlas-server"]
