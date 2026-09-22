# Build stage.
FROM rust:1.98.1-slim-trixie@sha256:ce84a5edd80c5f91e05c5533b1e53eb1da54028f33734dc06aa6b49fa190462d AS build
WORKDIR /src

# System libraries needed by the TLS and Matrix stacks.
#
# `git` is here for hooks/audit-cargo-deps.sh, not for the build: cargo-deny
# fetches the RustSec advisory database by spawning the git CLI, and without
# it the audit dies before running a single check. Its docs say the default
# is to fetch with `gix` and that git is only needed when
# `git-fetch-with-cli = true`; that is stale as of 0.20.2, where
# src/advisories/helpers/db.rs routes both settings through the same
# git-CLI path. Build stage only, so it never reaches the runtime image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev git \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates/partylinepager-core/Cargo.toml crates/partylinepager-core/Cargo.toml
COPY crates/partylinepagerd/Cargo.toml crates/partylinepagerd/Cargo.toml
COPY crates/partylinepagerctl/Cargo.toml crates/partylinepagerctl/Cargo.toml

# Build once against stub sources so this layer, keyed only on the Cargo.tomls
# above, caches every dependency (matrix-sdk, ring, aws-lc-sys...). Without
# buildx on this host there is no --mount=type=cache, so the layer cache is
# the only thing standing between a one-line edit and a from-scratch release
# build of the whole dependency tree.
RUN mkdir -p crates/partylinepager-core/src crates/partylinepagerd/src crates/partylinepagerctl/src \
 && echo "" > crates/partylinepager-core/src/lib.rs \
 && echo "" > crates/partylinepagerd/src/lib.rs \
 && printf 'fn main() {}\n' > crates/partylinepagerd/src/main.rs \
 && printf 'fn main() {}\n' > crates/partylinepagerctl/src/main.rs \
 && cargo build --release --workspace --bins

COPY crates ./crates
# Docker's COPY can leave these with the same mtime cargo saw during the stub
# build above, which fools cargo's freshness check into skipping the real
# compile entirely. Force every source file to look newer.
RUN find crates -name '*.rs' -exec touch {} + \
 && cargo build --release --workspace --bins

# The `yopass` CLI, for Link mode credential delivery (see policy.toml's
# `creds_delivery`). Built from source and pinned to a tag rather than
# `go install .../yopass@latest`: upstream publishes no prebuilt release
# binaries, and an unpinned build would silently pull in whatever upstream
# shipped since this image was last built. Bump YOPASS_VERSION by hand to
# update; `hooks/check-yopass-version.sh` checks it against upstream's latest.
#
# Built via `git clone <tag> && go build`, not `go install ...@vX.Y.Z`:
# upstream's release tags are unprefixed ("14.8.0", not "v14.8.0"), and Go's
# module version resolver only recognizes "v"-prefixed tags as versions, so
# `@v14.8.0` fails to resolve no matter how it's spelled. A tag-scoped clone
# sidesteps Go's module versioning entirely and pins to the exact same
# human-readable tag GitHub's release list and the check script both use.
FROM golang:1.27.1-bookworm@sha256:648f440f42a0958804efb24df176f806f9d353b41f1c0627f666428e40310f6b AS yopass-build
ARG YOPASS_VERSION=14.9.0
RUN git clone --depth 1 --branch "${YOPASS_VERSION}" \
        https://github.com/jhaals/yopass.git /src/yopass \
 && cd /src/yopass \
 && go build -o /go/bin/yopass ./cmd/yopass

# The Docker CLI and its compose plugin come from the official image rather than
# from Debian, which packages neither under a stable name.
FROM docker:29.8.0-cli@sha256:eccaacfeed644c7de222ff047483568cb988dde95476fbaaf10ea2d04921bb66 AS dockercli

# Runtime stage.
#
# The Docker CLI is here because the provider hooks drive it. That is also why
# this container is handed the Docker socket, which is equivalent to root on the
# host: see the security section of the README.
FROM debian:trixie-20260824-slim@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*

COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-compose \
                      /usr/local/libexec/docker/cli-plugins/docker-compose

COPY --from=build /src/target/release/partylinepagerd /usr/local/bin/partylinepagerd
COPY --from=build /src/target/release/partylinepagerctl /usr/local/bin/partylinepagerctl
COPY --from=yopass-build /go/bin/yopass /usr/local/bin/yopass
COPY hooks /opt/partylinepager/hooks
RUN chmod +x /opt/partylinepager/hooks/*.sh

RUN useradd -r -s /usr/sbin/nologin -d /nonexistent partyline
USER partyline

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/partylinepagerd"]
