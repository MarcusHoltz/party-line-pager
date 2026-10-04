# Build stage.
FROM rust:1.98.1-slim-trixie AS build
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
 && apt-get install -y --no-install-recommends pkg-config libssl-dev git cmake \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates/party-line-pager-core/Cargo.toml crates/party-line-pager-core/Cargo.toml
COPY crates/party-line-pagerd/Cargo.toml crates/party-line-pagerd/Cargo.toml
COPY crates/party-line-pagerctl/Cargo.toml crates/party-line-pagerctl/Cargo.toml

# Build once against stub sources so this layer, keyed only on the Cargo.tomls
# above, caches every dependency (matrix-sdk, ring, aws-lc-sys...). Without
# buildx on this host there is no --mount=type=cache, so the layer cache is
# the only thing standing between a one-line edit and a from-scratch release
# build of the whole dependency tree.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
ARG CMAKE_BUILD_PARALLEL_LEVEL=2
ENV CMAKE_BUILD_PARALLEL_LEVEL=${CMAKE_BUILD_PARALLEL_LEVEL}
RUN mkdir -p crates/party-line-pager-core/src crates/party-line-pagerd/src crates/party-line-pagerctl/src \
 && echo "" > crates/party-line-pager-core/src/lib.rs \
 && echo "" > crates/party-line-pagerd/src/lib.rs \
 && printf 'fn main() {}\n' > crates/party-line-pagerd/src/main.rs \
 && printf 'fn main() {}\n' > crates/party-line-pagerctl/src/main.rs \
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
FROM golang:1.27.1-bookworm AS yopass-build
ARG YOPASS_VERSION=14.9.0
RUN git clone --depth 1 --branch "${YOPASS_VERSION}" \
        https://github.com/jhaals/yopass.git /src/yopass \
 && cd /src/yopass \
 && go build -o /go/bin/yopass ./cmd/yopass

# The Docker CLI and its compose plugin come from the official image rather than
# from Debian, which packages neither under a stable name.
FROM docker:29.8.0-cli AS dockercli

# Runtime stage.
#
# The Docker CLI is here because the provider hooks drive it. That is also why
# this container is handed the Docker socket, which is equivalent to root on the
# host: see the security section of the README.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get upgrade -y \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && apt-get autoremove -y \
 && apt-get clean \
 && rm -rf /var/lib/apt/lists/*

COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-compose \
                      /usr/local/libexec/docker/cli-plugins/docker-compose

COPY --from=build /src/target/release/party-line-pagerd /usr/local/bin/party-line-pagerd
COPY --from=build /src/target/release/party-line-pagerctl /usr/local/bin/party-line-pagerctl
COPY --from=yopass-build /go/bin/yopass /usr/local/bin/yopass
COPY hooks /opt/party-line-pager/hooks
RUN chmod +x /opt/party-line-pager/hooks/*.sh

ARG PLP_VERSION=0.1.0
LABEL org.opencontainers.image.version="${PLP_VERSION}" \
      org.opencontainers.image.title="party-line-pager" \
      org.opencontainers.image.description="PartyLinePager modular image"

RUN useradd -r -s /usr/sbin/nologin -d /nonexistent partyline
USER partyline

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/party-line-pagerd"]
