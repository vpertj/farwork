# syntax=docker/dockerfile:1
#
# One container holds the whole product: the hub binary serves /api/v2 (REST +
# WebSocket), /healthz, /install.sh *and* the SvelteKit SPA, because it is started
# with FARWORK_HUB_STATIC_DIR pointing at the adapter-static output. No nginx, no
# sidecar, no shared volume between containers.

# --- 1. web: build apps/web ---------------------------------------------------
# apps/web carries its own pnpm-workspace.yaml, which makes it a pnpm workspace
# root of its own (pnpm 11 only honours `allowBuilds` there — AGENTS.md §3). Every
# command therefore runs *inside* apps/web: pnpm at the repository root would
# resolve a different workspace and `--frozen-lockfile` would fail against
# apps/web/pnpm-lock.yaml. The pinned pnpm version must match CI.
FROM node:24-bookworm-slim AS web
# No corepack: the version is pinned in one place (this line) and must stay equal to
# the pnpm/action-setup version in .github/workflows/ci.yml.
RUN npm install --global pnpm@11.19.0
WORKDIR /src/apps/web
COPY apps/web/package.json apps/web/pnpm-lock.yaml apps/web/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY apps/web/ ./
RUN pnpm run build

# --- 2. rust: build the hub binary --------------------------------------------
# rustup installs the channel pinned in rust-toolchain.toml; `rustup show` does it
# in its own layer so a toolchain bump is one cached download, not a silent switch.
FROM rust:1-bookworm AS rust
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# install.sh is `include_str!`-embedded by crates/farwork-hub/src/install.rs, so the
# build needs the repository file even though the runtime image does not.
COPY install.sh ./
# No database is needed at build time: the hub uses sqlx::migrate! (files embedded
# at compile time), not the query!/query_as! macros that read DATABASE_URL.
RUN cargo build --release -p farwork-hub

# --- 3. runtime ---------------------------------------------------------------
# Slim, not distroless: a shell is a hard requirement here (HEALTHCHECK, 1Panel's
# probes, `docker exec ... sh` during incidents), and distroless has none.
FROM debian:bookworm-slim AS runtime
# curl is what the HEALTHCHECK uses; ca-certificates is the trust store the hub and
# every connector it relays for need for outbound TLS.
RUN apt-get update \
 && apt-get install --no-install-recommends -y ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /srv \
 && useradd --system --uid 10001 --home-dir /srv --shell /usr/sbin/nologin farwork
COPY --from=rust /src/target/release/farwork-hub /usr/local/bin/farwork-hub
COPY --from=web /src/apps/web/build /srv/web

# The static root is baked in; the port matches the binary's own default (7300), so
# `docker run -p 13080:7300` works with no extra flags. DATABASE_URL and
# FARWORK_HUB_PUBLIC_URL are deliberately *not* set: the database is the operator's
# input, and the public URL is the only value that keeps emitted pairing commands
# dialable (see crates/farwork-hub/src/config.rs).
ENV FARWORK_HUB_STATIC_DIR=/srv/web \
    FARWORK_HUB_PORT=7300

WORKDIR /srv
USER farwork
EXPOSE 7300

# `-f` so a 5xx marks the container unhealthy instead of looking fine; the start
# period covers the first sqlx migration run against a fresh database.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${FARWORK_HUB_PORT}/healthz"

ENTRYPOINT ["/usr/local/bin/farwork-hub"]
