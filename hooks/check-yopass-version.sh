#!/bin/sh
# Reports whether the yopass CLI pinned in the Dockerfile is behind upstream.
#
# Not part of any hook contract partylinepagerd calls: there is no CI in this repo
# to wire it into yet, so this is a standalone check an admin runs by hand
# (or points a cron/CI job at later). It never touches the image; bumping the
# pin in the Dockerfile is a manual edit, on purpose, so a yopass release
# never changes what a build produces without someone deciding to take it.
#
# exit 0  pinned tag matches the latest release
# exit 1  pinned tag is behind, or either version could not be determined

set -eu

DOCKERFILE="${DOCKERFILE:-$(dirname "$0")/../Dockerfile}"

log() { echo "check-yopass-version: $*" >&2; }

# Upstream's release tags are unprefixed ("14.8.0", not "v14.8.0" — this
# is also why the Dockerfile clones the tag rather than using
# `go install ...@vX.Y.Z`, which cannot resolve an unprefixed tag at all).
# Both sides of this comparison must stay bare, or they can never match.
pinned=$(grep -o 'YOPASS_VERSION=[0-9][0-9.]*' "$DOCKERFILE" | sed 's/.*=//') || true
if [ -z "$pinned" ]; then
    log "could not find a pinned YOPASS_VERSION in $DOCKERFILE"
    exit 1
fi

latest=$(curl -fsSL https://api.github.com/repos/jhaals/yopass/releases/latest \
    | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/') || true
if [ -z "$latest" ]; then
    log "could not fetch the latest release tag from GitHub"
    exit 1
fi

if [ "$pinned" != "$latest" ]; then
    log "pinned $pinned is behind latest $latest; bump the tag in $DOCKERFILE"
    exit 1
fi

log "pinned $pinned matches latest"
