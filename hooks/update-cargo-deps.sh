#!/bin/sh
# Bumps Cargo.lock the safe way: cargo-cooldown refuses to adopt anything
# published more recently than cooldown.toml's window, then this script runs
# hooks/audit-cargo-deps.sh against the result and puts the old lockfile back
# if that audit fails.
#
# This is where the publish-age control actually lives. `cargo cooldown
# update` is the only cargo-cooldown command that audit-cargo-deps.sh's
# read-only contract allows, because per cargo-cooldown's docs `update`
# resolves and cools the lockfile in a temporary workspace copy and never
# compiles anything or runs a dependency build script.
#
# Not part of any hook contract party-line-pagerd calls: there is no CI in this
# repo to wire it into yet, so this is a standalone script an admin runs by
# hand, same standing as check-yopass-version.sh.
#
# This is the only script in the repo that rewrites Cargo.lock outside of an
# admin manually running `cargo add`/`cargo update` themselves. Read it
# before running it.
#
# cargo-cooldown is pinned to an exact version and installed under its own
# --root, for the same reason cargo-deny is: see audit-cargo-deps.sh.
#
# Cargo.lock is snapshotted before anything runs and restored on every
# failure path, including Ctrl-C, so this script either leaves a lockfile
# that passed the audit or leaves the one you started with. It never leaves a
# lockfile that failed.
#
# Two things about how cargo-cooldown works are worth knowing before running
# this, both from its docs/resolution-flow.md:
#
#   - While it works it renames the real Cargo.lock aside and parks an
#     "invalid sentinel Cargo.lock" in the workspace. If this script is
#     killed outright (SIGKILL, power loss) the traps below cannot run and
#     the repo can be left holding that sentinel, plus a leftover backup
#     file next to it. `git checkout -- Cargo.lock` is the recovery. Do not
#     expect `git status` to point at the leftover: its exact name is not
#     documented, and if it is `Cargo.lock.bak` then .gitignore and
#     .dockerignore already list it, so it stays invisible. `ls Cargo.lock*`
#     is the way to see it.
#   - It copies the workspace to a temp directory to resolve. That copy
#     "skips heavy generated directories such as .git and target", but the
#     docs do not say it skips ./.cache, which in this deployment is the
#     cargo registry and can be several GB. If this run fails on disk space
#     inside the container, that is the first thing to check. Exercised on
#     2026-08-28 with ./.cache at about 700 MB and it completed fine, so
#     whatever it copies is not fatal at that size. Not tested at several GB.
#
# One non-obvious failure mode: the post-update audit runs cargo-deny with
# --locked, so if the lockfile cargo-cooldown produced is one cargo would
# immediately want to change again, the audit fails and this script reverts.
# That is the intended direction. A lockfile cargo does not consider settled
# is not one to commit.
#
# exit 0  Cargo.lock passed the audit (it was either updated or already current)
# exit 1  something failed; Cargo.lock is byte-for-byte what it was before

set -eu

CARGO_COOLDOWN_VERSION=0.3.4

ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
LOCK="$ROOT/Cargo.lock"
COMPOSE="docker compose -f compose/docker-compose.build-tools.yml"

log() { echo "update-cargo-deps: $*" >&2; }

if [ ! -f "$LOCK" ]; then
    log "no Cargo.lock at $LOCK; nothing to update"
    exit 1
fi

# See audit-cargo-deps.sh for why every `docker compose run` here redirects
# stdin from /dev/null: `-T` does not stop the container consuming this
# script's stdin.
#
# See audit-cargo-deps.sh: tell a dead Docker daemon apart from a real
# finding, and refuse to run as root, before anything touches the lockfile.
# The root case matters more here than there: a container running as root
# writes Cargo.lock as root, and then restore() below cannot overwrite it as
# you, which silently breaks this script's one promise.
PROBE_UID="$( cd "$ROOT" && $COMPOSE run --rm -T build-tools id -u </dev/null 2>/dev/null | tr -d ' \t\r\n' )" || PROBE_UID=""

if [ -z "$PROBE_UID" ]; then
    log "cannot start the build-tools container; Cargo.lock is untouched"
    log "run this to see why: $COMPOSE run --rm -T build-tools true"
    exit 1
fi

if [ "$PROBE_UID" = "0" ]; then
    log "the build-tools container would run as root; Cargo.lock is untouched"
    log "it would write Cargo.lock as root and restore could not undo it"
    log "fix: run ./party-line-pager.sh, which writes HOST_UID and HOST_GID to .env,"
    log "     or set them by hand to your own id -u and id -g"
    exit 1
fi

# /var/tmp, not the default /tmp. On a machine where /tmp is a tmpfs the only
# copy of a known-good Cargo.lock would live in RAM for the length of a run
# that can take minutes, and would not survive the reboot that a wedged
# `docker compose` invites. /var/tmp is on disk and is the FHS location for
# exactly this.
SNAPSHOT="$(TMPDIR=/var/tmp mktemp)"
cp "$LOCK" "$SNAPSHOT"

# Set by restore() when it could not put the lockfile back, so the cleanup
# traps keep the snapshot instead of deleting the only good copy.
SNAPSHOT_KEEP=0

# cargo-cooldown restores the original lockfile itself when
# incompatible-publish-age = "deny" fails, but only for that one failure
# mode. Restoring from our own snapshot covers every other way this can end
# badly, and is a no-op when cargo-cooldown already did the right thing.
#
# A failed restore is the worst outcome this script has, so it is loud rather
# than fatal: every caller of restore() is already on its way to exit 1 with
# an explanation, and letting `set -e` kill the script on the cp would replace
# that explanation with silence at the exact moment the lockfile is wrong.
restore() {
    if cmp -s "$SNAPSHOT" "$LOCK"; then
        return 0
    fi
    if cp "$SNAPSHOT" "$LOCK"; then
        log "Cargo.lock restored to its pre-update contents"
    else
        log "COULD NOT RESTORE Cargo.lock. The working copy is modified and wrong."
        log "your pre-update lockfile is at: $SNAPSHOT"
        log "recover with: git checkout -- Cargo.lock"
        SNAPSHOT_KEEP=1
    fi
}

# Restore before cleaning up, not after. A plain `trap cleanup EXIT INT TERM`
# would delete the snapshot on Ctrl-C and then let the script keep running
# with nothing left to restore from, which is the one moment the snapshot is
# most needed: cargo-cooldown deliberately parks an invalid sentinel
# Cargo.lock in the workspace while it works, so an interrupted run is
# exactly when the repo can be left holding a lockfile that is not a
# lockfile. POSIX sh does not exit on its own after an INT trap, so these
# exit explicitly.
cleanup() { [ "$SNAPSHOT_KEEP" -eq 1 ] || rm -f "$SNAPSHOT"; }

trap 'cleanup' EXIT
trap 'echo >&2; log "interrupted"; restore; cleanup; exit 130' INT
trap 'log "terminated"; restore; cleanup; exit 143' TERM

log "running cargo cooldown update $CARGO_COOLDOWN_VERSION against Cargo.lock"

set +e
( cd "$ROOT" && $COMPOSE run --rm -T build-tools sh -c '
    set -u
    ver="$1"
    if [ -z "${CARGO_HOME:-}" ]; then
        echo "update-cargo-deps: CARGO_HOME is not set in the container; expected compose/docker-compose.build-tools.yml to set it" >&2
        exit 90
    fi
    root="$CARGO_HOME/pinned/cargo-cooldown-$ver"
    export PATH="$root/bin:$PATH"

    # cargo-cooldown keeps a cache under $HOME/.cache. The container runs as
    # ${HOST_UID} with no matching passwd entry, so HOME is "/" and it tries
    # to create /.cache, which it cannot write:
    #
    #   Error: failed to create cache directory /.cache/cargo-cooldown
    #
    # Point HOME at the bind-mounted cargo cache, which is already writable by
    # this uid and already the place this deployment keeps regenerable state.
    # Set here rather than in compose/docker-compose.build-tools.yml so that `cargo test` is
    # not handed a different HOME as a side effect. cargo-deny needs none of
    # this, which is why audit-cargo-deps.sh does not do it.
    HOME="$CARGO_HOME/home"
    export HOME
    mkdir -p "$HOME" || { echo "update-cargo-deps: could not create $HOME" >&2; exit 90; }

    if [ ! -x "$root/bin/cargo-cooldown" ]; then
        echo "update-cargo-deps: installing pinned cargo-cooldown $ver (first run only)" >&2
        cargo install --locked --version "=$ver" --root "$root" cargo-cooldown \
            || { echo "update-cargo-deps: could not install cargo-cooldown $ver" >&2; exit 90; }
    fi

    cargo cooldown --version >&2 || exit 90

    cargo cooldown update
' sh "$CARGO_COOLDOWN_VERSION" </dev/null )
rc=$?
set -e

case $rc in
    0) : ;;
    90) restore; log "could not install or run cargo-cooldown; Cargo.lock is at its pre-update contents"; exit 1 ;;
    *) restore; log "cargo cooldown update failed or declined (exit $rc); Cargo.lock is at its pre-update contents"; exit 1 ;;
esac

log "update finished, auditing the result"

if ! "$ROOT/hooks/audit-cargo-deps.sh"; then
    restore
    log "post-update audit failed; the updated Cargo.lock was discarded"
    exit 1
fi

if cmp -s "$SNAPSHOT" "$LOCK"; then
    log "Cargo.lock was already current and passed the audit"
else
    log "Cargo.lock updated and passed the audit"
fi
