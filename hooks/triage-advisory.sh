#!/bin/sh
# Diagnoses cargo-deny advisory failures and helps resolve them.
#
# When audit-cargo-deps.sh (Maintenance menu item 5) fails on advisories,
# run this script. For each RUSTSEC finding it checks whether a compatible
# update exists and either directs you to update-cargo-deps.sh or offers
# to add an ignore entry to deny.toml with a justification comment.
#
# The decision tree per advisory:
#
#   cargo update -p <pkg> --dry-run
#     |
#     +-- update available --> "run update-cargo-deps.sh (menu item 6)"
#     |
#     +-- blocked (0 packages) --> offer to add ignore to deny.toml
#
# Blocked means the fix version is outside the semver range allowed by a
# parent dependency. This is common with transitive deps pinned by crates
# like matrix-sdk. The only resolution is to wait for upstream or ignore.
#
# Pass -y to auto-accept all ignore additions (non-interactive mode).
#
# CARGO_DENY_VERSION must match audit-cargo-deps.sh.
#
# exit 0  all findings resolved, re-audit passes
# exit 1  setup failure, user cancelled, or audit still failing

set -eu

CARGO_DENY_VERSION=0.20.2

ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
COMPOSE="docker compose -f compose/docker-compose.build-tools.yml"
DENY_TOML="$ROOT/deny.toml"

AUTO_YES=0
if [ "${1:-}" = "-y" ]; then AUTO_YES=1; fi

log() { printf 'triage: %s\n' "$*" >&2; }
hr()  { printf '%.0s-' 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; printf '\n'; }

# ---- Pre-flight checks ------------------------------------------------

if [ ! -f "$DENY_TOML" ]; then
    log "no deny.toml at $DENY_TOML"
    exit 1
fi

if ! grep -q 'ignore = \[' "$DENY_TOML"; then
    log "deny.toml has no ignore = [ list; cannot add entries"
    log "add an empty ignore list to [advisories] first"
    exit 1
fi

PROBE_UID="$( cd "$ROOT" && $COMPOSE run --rm -T build-tools id -u </dev/null 2>/dev/null | tr -d ' \t\r\n' )" || PROBE_UID=""
if [ -z "$PROBE_UID" ]; then
    log "cannot start the build-tools container"
    log "run: $COMPOSE run --rm -T build-tools true"
    exit 1
fi
if [ "$PROBE_UID" = "0" ]; then
    log "container runs as root; set HOST_UID/HOST_GID in .env"
    log "run ./party-line-pager.sh once, or: echo HOST_UID=\$(id -u) >> .env"
    exit 1
fi

# ---- Run cargo-deny ---------------------------------------------------

TMPOUT="$(TMPDIR=/var/tmp mktemp)"
TMPINS="$(TMPDIR=/var/tmp mktemp)"
trap 'rm -f "$TMPOUT" "$TMPINS"' EXIT

log "running cargo-deny $CARGO_DENY_VERSION..."

set +e
( cd "$ROOT" && $COMPOSE run --rm -T build-tools sh -c '
    set -u
    ver="$1"
    if [ -z "${CARGO_HOME:-}" ]; then exit 90; fi
    root="$CARGO_HOME/pinned/cargo-deny-$ver"
    export PATH="$root/bin:$PATH"
    if [ ! -x "$root/bin/cargo-deny" ]; then
        echo "triage: installing cargo-deny $ver (first run)" >&2
        cargo install --locked --version "=$ver" --root "$root" cargo-deny \
            || exit 90
    fi
    cargo deny --locked fetch db >&2 || exit 91
    cargo deny --locked check advisories bans sources
' sh "$CARGO_DENY_VERSION" </dev/null ) > "$TMPOUT" 2>&1
rc=$?
set -e

case $rc in
    0) log "clean: no findings"; exit 0 ;;
    90) log "cargo-deny setup failed"; cat "$TMPOUT"; exit 1 ;;
    91) log "advisory database fetch failed"; cat "$TMPOUT"; exit 1 ;;
esac

# ---- Identify what failed ----------------------------------------------

if grep -q 'bans FAILED' "$TMPOUT" 2>/dev/null; then
    log "note: bans also failed; this script handles advisories only"
    log "bans failures require manual deny.toml [bans].deny edits"
fi
if grep -q 'sources FAILED' "$TMPOUT" 2>/dev/null; then
    log "note: sources also failed; this script handles advisories only"
    log "sources failures mean a dep comes from outside crates.io"
fi

if ! grep -q 'advisories FAILED' "$TMPOUT" 2>/dev/null; then
    log "audit failed but not on advisories; manual fix needed"
    grep 'FAILED\|ok' "$TMPOUT" | tail -1
    exit 1
fi

# ---- Parse advisory IDs ------------------------------------------------

IDS=$(grep 'ID: RUSTSEC-' "$TMPOUT" | sed 's/.*ID: //' | tr -d ' \r')

if [ -z "$IDS" ]; then
    log "advisories FAILED but no RUSTSEC IDs found in output"
    log "full output:"
    cat "$TMPOUT"
    exit 1
fi

COUNT=$(echo "$IDS" | wc -l | tr -d ' ')
log "found $COUNT advisory finding(s)"

# ---- Diagnose each advisory --------------------------------------------

NEED_UPDATE=""
NEED_IGNORE=""

for ID in $IDS; do
    if grep -q "\"$ID\"" "$DENY_TOML" 2>/dev/null; then
        log "$ID already in deny.toml (stale advisory-db cache?)"
        continue
    fi

    PKG=$(grep -A 20 "ID: $ID" "$TMPOUT" \
        | grep 'cargo update -p' \
        | head -1 \
        | sed 's/.*cargo update -p //' \
        | sed 's/[^a-zA-Z0-9_-].*//')

    DESC=$(grep -B 20 "ID: $ID" "$TMPOUT" \
        | grep 'error\[' \
        | tail -1 \
        | sed 's/.*]: //' \
        | cut -c1-70)

    SOL=$(grep -A 20 "ID: $ID" "$TMPOUT" \
        | grep 'Solution:' \
        | head -1 \
        | sed 's/.*Solution: //')

    echo ""
    hr
    printf '  %s\n' "$ID"
    printf '  Package:  %s\n' "${PKG:-unknown}"
    printf '  Issue:    %s\n' "${DESC:-no description}"
    printf '  Fix:      %s\n' "${SOL:-none suggested}"

    if [ -z "$PKG" ]; then
        printf '  Status:   cannot determine package; manual fix needed\n'
        continue
    fi

    printf '  Checking: cargo update -p %s --dry-run ...\n' "$PKG"
    set +e
    DRY_OUT=$( cd "$ROOT" && $COMPOSE run --rm -T build-tools \
        cargo update -p "$PKG" --dry-run </dev/null 2>&1 )
    set -e

    if echo "$DRY_OUT" | grep -q 'Locking 0 packages'; then
        printf '  Result:   BLOCKED (no compatible update in current dep tree)\n'
        NEED_IGNORE="$NEED_IGNORE $ID:$PKG"
    elif echo "$DRY_OUT" | grep -Fq "$PKG"; then
        UPDATED=$(echo "$DRY_OUT" | grep -F "$PKG" | head -1 \
            | sed 's/^[[:space:]]*//')
        printf '  Result:   UPDATE AVAILABLE: %s\n' "$UPDATED"
        NEED_UPDATE="$NEED_UPDATE $PKG"
    else
        printf '  Result:   unexpected; assuming blocked\n'
        NEED_IGNORE="$NEED_IGNORE $ID:$PKG"
    fi
done

echo ""

# ---- Report updatable packages -----------------------------------------

if [ -n "$NEED_UPDATE" ]; then
    hr
    printf '  Packages with updates available:\n'
    for pkg in $NEED_UPDATE; do
        printf '    - %s\n' "$pkg"
    done
    echo ""
    printf '  Run: ./hooks/update-cargo-deps.sh\n'
    printf '  Or:  Maintenance menu > item 6 (Update dependencies)\n'
    hr
    echo ""
fi

# ---- Add ignores for blocked packages -----------------------------------

CHANGES_MADE=0

if [ -n "$NEED_IGNORE" ]; then
    hr
    printf '  Advisories blocked by transitive deps:\n'
    for entry in $NEED_IGNORE; do
        id=$(echo "$entry" | cut -d: -f1)
        pkg=$(echo "$entry" | cut -d: -f2)
        printf '    - %s (%s)\n' "$id" "$pkg"
    done
    echo ""

    if [ "$AUTO_YES" -eq 1 ]; then
        ans="y"
    elif [ -t 0 ] || [ -e /dev/tty ]; then
        printf '  Add ignore entries to deny.toml? [Y/n] '
        read -r ans < /dev/tty 2>/dev/null || ans="n"
    else
        log "no terminal and -y not passed; skipping deny.toml edits"
        ans="n"
    fi

    case "$ans" in
        n|N|no|No)
            printf '  Skipped. Add them manually or re-run with -y.\n'
            ;;
        *)
            for entry in $NEED_IGNORE; do
                id=$(echo "$entry" | cut -d: -f1)
                pkg=$(echo "$entry" | cut -d: -f2)

                IGNORE_LINE=$(grep -n 'ignore = \[' "$DENY_TOML" \
                    | head -1 | cut -d: -f1)
                CLOSE_OFFSET=$(tail -n +"$IGNORE_LINE" "$DENY_TOML" \
                    | grep -n '^]' | head -1 | cut -d: -f1)
                if [ -z "$IGNORE_LINE" ] || [ -z "$CLOSE_OFFSET" ]; then
                    log "cannot locate ignore array boundaries in deny.toml"
                    log "add the ignore for $id by hand"
                    continue
                fi
                CLOSE_LINE=$((IGNORE_LINE + CLOSE_OFFSET - 1))

                {
                    head -n $((CLOSE_LINE - 1)) "$DENY_TOML"
                    printf '    # %s: transitive dep, no compatible update.\n' "$pkg"
                    printf '    "%s",\n' "$id"
                    tail -n +"$CLOSE_LINE" "$DENY_TOML"
                } > "$TMPINS"

                # The temp file must be longer than the original (we added
                # two lines). If it is shorter or empty, something went
                # wrong and overwriting deny.toml would lose data.
                orig_lines=$(wc -l < "$DENY_TOML")
                new_lines=$(wc -l < "$TMPINS")
                if [ "$new_lines" -le "$orig_lines" ]; then
                    log "temp file is not longer than deny.toml ($new_lines <= $orig_lines); refusing to overwrite"
                    log "deny.toml is unchanged; add the ignore by hand"
                    continue
                fi
                cp "$TMPINS" "$DENY_TOML"

                printf '  Added %s (%s)\n' "$id" "$pkg"
                CHANGES_MADE=1
            done
            ;;
    esac
    hr
    echo ""
fi

# ---- Verify -------------------------------------------------------------

if [ "$CHANGES_MADE" -eq 1 ]; then
    log "re-running audit to verify..."
    echo ""
    if "$ROOT/hooks/audit-cargo-deps.sh"; then
        echo ""
        log "audit passes"
        log "commit deny.toml and push to resolve the CI failure"
        exit 0
    else
        echo ""
        log "audit still failing; more findings may remain"
        log "run this script again to triage remaining findings"
        exit 1
    fi
elif [ -n "$NEED_UPDATE" ]; then
    log "run update-cargo-deps.sh first, then re-run this script"
    exit 1
else
    log "no changes made"
    exit 1
fi
