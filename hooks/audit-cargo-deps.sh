#!/bin/sh
# Checks the committed Cargo.lock against cargo-deny: the RustSec advisory
# database, deny.toml's banned-package list, and source pinning. Read-only:
# it reports, and never compiles or downloads dependency sources.
#
# cargo-deny is invoked with `--locked`. That matters and is not decoration:
# cargo-deny builds its graph by running `cargo metadata`, which will happily
# rewrite Cargo.lock if resolution would change it. `--locked` makes cargo
# error out instead, so this script cannot modify the lockfile it is auditing
# even when Cargo.lock and Cargo.toml have drifted apart. If it fails with a
# lockfile-needs-updating error, that is the drift talking, not a finding.
#
# cargo-cooldown is deliberately NOT run here. Its `check` subcommand is not
# an auditor: per its own docs it is a guard wrapper that rewrites Cargo.lock
# ("publish the final temp Cargo.lock back to the real workspace") and then
# runs `cargo check`, which downloads dependency sources and executes their
# build scripts. Running that as part of an audit would mean compiling a
# dependency graph that may be exactly the thing being audited. It is also
# useless as a check: with cooldown.toml's `lockfile-baseline = "floor"`,
# versions already present in Cargo.lock are the protected baseline, so a
# guard pass over an unchanged lockfile has nothing to cool and always
# passes. Publish-age is enforced at update time only, by
# hooks/update-cargo-deps.sh.
#
# Not part of any hook contract partylinepagerd calls: there is no CI in this
# repo to wire it into yet, so this is a standalone check an admin runs by
# hand (or points a cron/CI job at later), same standing as
# check-yopass-version.sh.
#
# deny.toml's [bans] list is the direct defense against a compromised crate
# like the arrayref@0.3.10 supply-chain attack (2026-08-20): known-bad
# names/versions are blocked outright, before RustSec even has to publish an
# advisory.
#
# cargo-deny is pinned to an exact version and installed under its own
# --root, so the tool that defends against a crates.io compromise is not
# itself fetched as "whatever is newest today". Bump CARGO_DENY_VERSION by
# hand. The first run builds it from source inside the build-tools image and caches
# it in .cache/cargo/pinned, which takes a few minutes; every run after that
# reuses it. That build was exercised on a clean image on 2026-08-28 and
# needed one package the build stage did not install: `git`, which cargo-deny
# spawns to fetch the advisory database. See the Dockerfile.
#
# Fails closed. Each of these is a failure, not a skip: the container will not
# start, the container would run as root, deny.toml is missing, cargo-deny
# cannot be installed, or the advisory database cannot be fetched. This script
# never reports "clean" without cargo-deny having actually run to completion
# against this repo's config.
#
# exit 0  cargo-deny ran and passed
# exit 1  the container would not start, cargo-deny could not be installed or
#         could not fetch the advisory database, or the check reported
#         findings (the log line says which)

set -eu

CARGO_DENY_VERSION=0.20.2

ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
COMPOSE="docker compose -f compose/docker-compose.build-tools.yml"

log() { echo "audit-cargo-deps: $*" >&2; }

# cargo-deny does not fail when its config is missing: it warns and falls back
# to built-in defaults, which have an empty ban list and no source pinning. A
# run like that reports "clean" while checking almost nothing, which is the
# one wrong answer this script must never give. Checked before the container
# probe below, since it costs nothing and the probe does not.
if [ ! -f "$ROOT/deny.toml" ]; then
    log "no deny.toml at $ROOT/deny.toml; refusing to audit against cargo-deny's defaults"
    log "without it the ban list and source pinning are silently empty"
    exit 1
fi

# Every `docker compose run` below redirects stdin from /dev/null. `-T` only
# stops compose allocating a TTY; it still attaches the container's stdin to
# this script's, and the container then consumes it to EOF. Two ways that
# bites: run from the maintenance menu, the container eats the keystrokes the
# menu's next `read` was waiting for; run the script itself down a pipe, and
# it eats whatever the outer shell had left to read, which is how fragments of
# this file's own source end up printed on a terminal.
#
# Separate the "docker could not run anything" case from the "cargo-deny
# found something" case up front. Both otherwise surface as exit 1 from
# `docker compose run`, and reporting a dead Docker daemon as a security
# finding sends you looking in the wrong place.
#
# The probe asks the container who it is rather than just running `true`,
# because the same call answers the second question for free. The container
# runs as ${HOST_UID}:${HOST_GID}, which compose/docker-compose.build-tools.yml defaults to
# 0:0, and .env is not in git: on a fresh clone, or with HOST_UID exported
# empty, cargo runs as root and root-owns ./target and ./.cache inside your
# own checkout, needing sudo to clear. Asking the container beats reproducing
# compose's precedence rules between the process environment and .env, which
# is where a hand-rolled check would eventually drift.
PROBE_UID="$( cd "$ROOT" && $COMPOSE run --rm -T build-tools id -u </dev/null 2>/dev/null | tr -d ' \t\r\n' )" || PROBE_UID=""

if [ -z "$PROBE_UID" ]; then
    log "cannot start the build-tools container; nothing was checked"
    log "run this to see why: $COMPOSE run --rm -T build-tools true"
    exit 1
fi

if [ "$PROBE_UID" = "0" ]; then
    log "the build-tools container would run as root; nothing was checked"
    log "it would root-own ./target and ./.cache in this checkout, needing sudo to clear"
    log "fix: run ./partylinepager.sh, which writes HOST_UID and HOST_GID to .env,"
    log "     or set them by hand to your own id -u and id -g"
    exit 1
fi

log "running cargo-deny $CARGO_DENY_VERSION against Cargo.lock"

set +e
( cd "$ROOT" && $COMPOSE run --rm -T build-tools sh -c '
    set -u
    ver="$1"

    # cargo install puts binaries under the --root given below, but the image
    # bakes PATH to the default CARGO_HOME (/usr/local/cargo/bin) and does not
    # follow the CARGO_HOME override from compose/docker-compose.build-tools.yml, so cargo
    # would never find cargo-deny as a subcommand without this.
    if [ -z "${CARGO_HOME:-}" ]; then
        echo "audit-cargo-deps: CARGO_HOME is not set in the container; expected compose/docker-compose.build-tools.yml to set it" >&2
        exit 90
    fi
    root="$CARGO_HOME/pinned/cargo-deny-$ver"
    export PATH="$root/bin:$PATH"

    # Test the pinned path directly rather than command -v: an unpinned
    # cargo-deny left in $CARGO_HOME/bin by an older revision of this script
    # would otherwise satisfy the check and silently defeat the pin.
    if [ ! -x "$root/bin/cargo-deny" ]; then
        echo "audit-cargo-deps: installing pinned cargo-deny $ver (first run only)" >&2
        cargo install --locked --version "=$ver" --root "$root" cargo-deny \
            || { echo "audit-cargo-deps: could not install cargo-deny $ver" >&2; exit 90; }
    fi

    cargo deny --version >&2 || exit 90

    # Fetch the advisory database as its own step, so that "could not get the
    # database" stops here with a sentinel instead of reaching the check and
    # being misread as a finding. cargo-deny spawns the git CLI to do this,
    # and the failure is otherwise indistinguishable by exit code from a real
    # advisories hit: see the case statement below.
    #
    # `fetch db`, never `fetch all` or `fetch index`. `index` fetches crate
    # sources, which is exactly the downloading this script promises not to do.
    cargo deny --locked fetch db >&2 \
        || { echo "audit-cargo-deps: could not fetch the advisory database" >&2; exit 91; }

    # Explicit check names, not bare `cargo deny check`: that would also run
    # the licenses check, and deny.toml defines no license policy on purpose.
    # --locked goes before the subcommand; it is a common option, not a
    # `check` option. See the header for why it is here.
    cargo deny --locked check advisories bans sources
' sh "$CARGO_DENY_VERSION" </dev/null )
rc=$?
set -e

# cargo-deny's exit code on findings is a bitset of the checks that failed
# (advisories 1, bans 2, licenses 4, sources 8: see stats_to_exit_code in
# src/cargo-deny/stats.rs), so it occupies 1-15. Its exit code when the tool
# itself fails is 1, which collides with an advisories-only finding. That
# collision is why the sentinels below are 90/91 rather than a value inside
# the bitset range, and why the advisory database is fetched as a separate
# step above: by the time `check` runs, exit 1 means advisories, not a dead
# tool.
case $rc in
    0) log "clean: no advisories, no banned packages, no source-pinning violations" ;;
    90) log "could not install or run cargo-deny; refusing to report a pass without it"; exit 1 ;;
    91) log "could not fetch the advisory database; nothing was checked"; exit 1 ;;
    *) log "cargo-deny reported findings (exit $rc); see the output above"; exit 1 ;;
esac
