#!/bin/bash
# PartyLinePager full image entrypoint.
#
# Startup order:
#   1. Create config dir, copy examples on first run
#   2. Generate config from env vars (env takes precedence)
#   3. Tor directory permissions
#   4. If PLP_HEADLESS=1 and config exists: exec party-line-pagerd (no wizard)
#   5. If PLP_AUTO_START=1 and config exists: start daemon in background
#   6. exec ttyd with the wizard as the browser terminal
set -euo pipefail

CONFIG_DIR="${CONFIG_DIR:-/config}"
POLICY_FILE="$CONFIG_DIR/policy.toml"
ADAPTERS_FILE="$CONFIG_DIR/adapters.toml"
STATE_DIR="${STATE_DIR:-$CONFIG_DIR/state}"

PLP_AUTO_START="${PLP_AUTO_START:-0}"
PLP_HEADLESS="${PLP_HEADLESS:-0}"
PLP_PROVIDERS="${PLP_PROVIDERS:-}"

TTYD_PORT="${TTYD_PORT:-7681}"
TTYD_INTERNAL_PORT="7682"

log() { echo "[entrypoint] $*"; }

# ── 1. First-run setup ───────────────────────────────────────────────
mkdir -p "$CONFIG_DIR" "$STATE_DIR"

# Copy each example separately, not both under one test. Gating both on
# policy.toml meant a volume that somehow had a policy but no adapters file
# never got one, and the daemon then failed on a missing path instead of on a
# missing credential, which is a much worse thing to hand somebody.
if [ ! -f "$POLICY_FILE" ]; then
    log "First run: copying example policy.toml to $CONFIG_DIR"
    cp /opt/party-line-pager/policy.example.toml "$POLICY_FILE"
fi
if [ ! -f "$ADAPTERS_FILE" ]; then
    log "First run: copying example adapters.toml to $CONFIG_DIR"
    cp /opt/party-line-pager/adapters.example.toml "$ADAPTERS_FILE"
fi

# ── 2. Env var config generation ─────────────────────────────────────
# Env vars take precedence over wizard-written config. If any adapter
# token is set, regenerate adapters.toml from env vars.

_has_adapter_env() {
    [ -n "${TELEGRAM_TOKEN:-}" ] || [ -n "${DISCORD_TOKEN:-}" ] \
    || [ -n "${MATRIX_HOMESERVER:-}" ] || [ -n "${IRC_SERVER:-}" ] \
    || [ -n "${XMPP_JID:-}" ] || [ -n "${MASTODON_INSTANCE:-}" ] \
    || [ -n "${MATTERMOST_URL:-}" ] || [ -n "${SIGNAL_NUMBER:-}" ] \
    || [ -n "${EMAIL_SMTP_HOST:-}" ]
}

if _has_adapter_env; then
    log "Adapter env vars detected, generating $ADAPTERS_FILE"
    : > "$ADAPTERS_FILE"
    chmod 600 "$ADAPTERS_FILE"

    if [ -n "${TELEGRAM_TOKEN:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[telegram]
enabled = true
token = "$TELEGRAM_TOKEN"
TOML
    fi

    if [ -n "${DISCORD_TOKEN:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[discord]
enabled = true
token = "$DISCORD_TOKEN"
TOML
    fi

    if [ -n "${MATRIX_HOMESERVER:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[matrix]
enabled = true
homeserver = "$MATRIX_HOMESERVER"
user = "${MATRIX_USER:-}"
password = "${MATRIX_PASSWORD:-}"
store_path = "${MATRIX_STORE_PATH:-/var/lib/party-line-pager/matrix}"
TOML
    fi

    if [ -n "${IRC_SERVER:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[irc]
enabled = true
server = "$IRC_SERVER"
port = ${IRC_PORT:-6697}
nick = "${IRC_NICK:-party-line-pager}"
tls = ${IRC_TLS:-true}
channels = ["${IRC_CHANNEL:-}"]
TOML
        [ -n "${IRC_ACCOUNT:-}" ] && echo "account = \"$IRC_ACCOUNT\"" >> "$ADAPTERS_FILE"
        [ -n "${IRC_PASSWORD:-}" ] && echo "password = \"$IRC_PASSWORD\"" >> "$ADAPTERS_FILE"
    fi

    if [ -n "${XMPP_JID:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[xmpp]
enabled = true
jid = "$XMPP_JID"
password = "${XMPP_PASSWORD:-}"
tls = ${XMPP_TLS:-true}
TOML
    fi

    if [ -n "${MASTODON_INSTANCE:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[mastodon]
enabled = true
base_url = "$MASTODON_INSTANCE"
access_token = "${MASTODON_TOKEN:-}"
poll_interval = "${MASTODON_POLL_INTERVAL:-30s}"
TOML
    fi

    if [ -n "${MATTERMOST_URL:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[mattermost]
enabled = true
base_url = "$MATTERMOST_URL"
access_token = "${MATTERMOST_TOKEN:-}"
TOML
    fi

    if [ -n "${SIGNAL_NUMBER:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[signal]
enabled = true
rest_url = "${SIGNAL_REST_URL:-http://127.0.0.1:8080}"
number = "$SIGNAL_NUMBER"
TOML
    fi

    if [ -n "${EMAIL_SMTP_HOST:-}" ]; then
        cat >> "$ADAPTERS_FILE" <<TOML
[email]
enabled = true
smtp_host = "$EMAIL_SMTP_HOST"
smtp_port = ${EMAIL_SMTP_PORT:-587}
smtp_user = "${EMAIL_SMTP_USER:-}"
smtp_password = "${EMAIL_SMTP_PASSWORD:-}"
imap_host = "${EMAIL_IMAP_HOST:-$EMAIL_SMTP_HOST}"
imap_port = ${EMAIL_IMAP_PORT:-993}
imap_user = "${EMAIL_IMAP_USER:-${EMAIL_SMTP_USER:-}}"
imap_password = "${EMAIL_IMAP_PASSWORD:-${EMAIL_SMTP_PASSWORD:-}}"
mailbox = "${EMAIL_MAILBOX:-INBOX}"
from = "${EMAIL_FROM:-${EMAIL_SMTP_USER:-}}"
tls = ${EMAIL_TLS:-true}
poll_interval = "${EMAIL_POLL_INTERVAL:-60s}"
TOML
    fi
fi

# Provider sections from env var.
if [ -n "$PLP_PROVIDERS" ]; then
    log "PLP_PROVIDERS=$PLP_PROVIDERS, updating provider sections"
    # Remove existing [provider.*] sections (everything from first
    # [provider. to the next top-level section or EOF).
    _tmp="$(mktemp)"
    awk '
        /^\[provider\./ { skip=1; next }
        /^\[/ && skip { skip=0 }
        !skip { print }
    ' "$POLICY_FILE" > "$_tmp"

    # Append enabled providers.
    IFS=',' read -ra _providers <<< "$PLP_PROVIDERS"
    for _p in "${_providers[@]}"; do
        _p="$(echo "$_p" | tr -d ' ')"
        case "$_p" in
            tor)
                cat >> "$_tmp" <<TOML

[provider.tor]
up = "/opt/party-line-pager/hooks/provider-tor.sh"
down = "/opt/party-line-pager/hooks/teardown-tor.sh"
TOML
                ;;
            i2p)
                cat >> "$_tmp" <<TOML

[provider.i2p]
up = "/opt/party-line-pager/hooks/provider-i2p.sh"
down = "/opt/party-line-pager/hooks/teardown-i2p.sh"
TOML
                ;;
            rns)
                cat >> "$_tmp" <<TOML

[provider.rns]
up = "/opt/party-line-pager/hooks/provider-rns.sh"
down = "/opt/party-line-pager/hooks/teardown-rns.sh"
TOML
                ;;
            web)
                cat >> "$_tmp" <<TOML

[provider.web]
up = "/opt/party-line-pager/hooks/provider-web.sh"
down = "/opt/party-line-pager/hooks/teardown-web.sh"
base_url = "${PLP_WEB_BASE_URL:-https://p2p.mirotalk.com}"
path = "${PLP_WEB_PATH:-join}"
TOML
                ;;
        esac
    done
    mv "$_tmp" "$POLICY_FILE"
fi

# ── 3. Tor directory permissions ─────────────────────────────────────
if [ -d /var/lib/tor ]; then
    chmod 700 /var/lib/tor /var/lib/tor/hidden_service 2>/dev/null || true
    chown -R debian-tor:debian-tor /var/lib/tor 2>/dev/null || true
fi

# ── 3a. Fix config ownership ────────────────────────────────────────
# The entrypoint runs as root so it can manage Tor, I2P, and RNS
# processes. But config files written here end up root-owned on the
# host bind mount, locking the user out of their own config dir.
# Chown everything back to the caller's uid/gid so the files stay
# theirs after the container exits.
#
# When HOST_UID is not passed, take it from the owner of the mounted config
# dir instead: /config is a bind mount of the host's ./config, so its uid is
# the host user. A fresh clone has that dir already, owned by whoever cloned
# it. Without this fallback the wizard would detect uid 0 inside the
# container, record that as the host identity, and every file it wrote would
# stay root-owned, defeating the handover above.
if [ -z "${HOST_UID:-}" ]; then
    _derived_uid="$(stat -c %u "$CONFIG_DIR" 2>/dev/null || true)"
    if [ -n "$_derived_uid" ] && [ "$_derived_uid" != "0" ]; then
        HOST_UID="$_derived_uid"
        HOST_GID="$(stat -c %g "$CONFIG_DIR" 2>/dev/null || true)"
        HOST_GID="${HOST_GID:-$_derived_uid}"
        log "HOST_UID not set, taking $HOST_UID:$HOST_GID from the owner of $CONFIG_DIR"
    fi
fi
if [ -n "${HOST_UID:-}" ] && [ "${HOST_UID:-0}" != "0" ]; then
    _owner="${HOST_UID}:${HOST_GID:-$HOST_UID}"
    chown -R "$_owner" "$CONFIG_DIR" 2>/dev/null || true
    log "config ownership set to $_owner"

    export HOST_UID HOST_GID
fi

# ── 3b. Start signal-cli-rest-api if Signal is configured ────────────
if [ -n "${SIGNAL_NUMBER:-}" ]; then
    _sigapi=""
    [ -x /usr/local/bin/signal-cli-rest-api ] && _sigapi=/usr/local/bin/signal-cli-rest-api
    if [ -n "$_sigapi" ]; then
        log "Starting signal-cli-rest-api in background (MODE=native)"
        mkdir -p "${CONFIG_DIR}/signal-cli"
        # Created after section 3a already ran, so it would land root-owned on
        # the host bind mount. signal-cli-rest-api itself runs as root and can
        # write to a directory owned by somebody else, so hand this one over
        # too rather than leaving it behind.
        chown "${_owner:-0:0}" "${CONFIG_DIR}/signal-cli" 2>/dev/null || true
        MODE=native \
            SIGNAL_CLI_CONFIG_DIR="${CONFIG_DIR}/signal-cli" \
            "$_sigapi" &
        log "signal-cli-rest-api started (pid $!)"
    else
        log "WARNING: signal-cli-rest-api binary not found, Signal adapter will not work"
    fi
    unset _sigapi
fi

# ── 4. Headless mode ─────────────────────────────────────────────────
# The daemon runs as root here, and has to: the tor, i2p and rns relays are
# vendored scripts that chown their state to a service user and setuid into it,
# so an unprivileged daemon cannot bring up three of the four room types. The
# web provider needs no relay and would work either way. State files are handed
# to the host user as they are written instead, in Store::save.
if [ "$PLP_HEADLESS" = "1" ] && [ -f "$ADAPTERS_FILE" ]; then
    log "Headless mode: starting party-line-pagerd as PID 1"
    exec /usr/local/bin/party-line-pagerd \
        --policy "$POLICY_FILE" \
        --adapters "$ADAPTERS_FILE" \
        --state "$STATE_DIR"
fi

# ── 5. Auto-start daemon in background ──────────────────────────────
if [ "$PLP_AUTO_START" = "1" ] && [ -f "$ADAPTERS_FILE" ]; then
    log "Auto-start: launching party-line-pagerd in background"
    /usr/local/bin/party-line-pagerd \
        --policy "$POLICY_FILE" \
        --adapters "$ADAPTERS_FILE" \
        --state "$STATE_DIR" &
    log "party-line-pagerd started (pid $!)"
fi

# ── 6. Launch ttyd with the wizard ───────────────────────────────────
ttyd_args=(
    --writable
    --port "$TTYD_INTERNAL_PORT"
    --interface 127.0.0.1
    --client-option titleFixed="PartyLinePager"
    --client-option disableLeaveAlert=true
    --client-option rendererType=canvas
)

if [ -n "${TTYD_CREDENTIAL:-}" ]; then
    ttyd_args+=(--credential "$TTYD_CREDENTIAL")
    log "ttyd basic auth enabled (user: ${TTYD_CREDENTIAL%%:*})"
else
    log "=================================================="
    log "WARNING: TTYD_CREDENTIAL is not set."
    log "The terminal has NO password. Anyone who can reach"
    log "port $TTYD_PORT can control this server."
    log "Set TTYD_CREDENTIAL=user:password and restart."
    log "=================================================="
fi

# If nginx is available, use it as TLS terminator. Otherwise serve
# ttyd directly on the external port.
if command -v nginx >/dev/null 2>&1; then
    log "Starting ttyd on 127.0.0.1:$TTYD_INTERNAL_PORT (nginx TLS on :$TTYD_PORT)"
else
    log "No nginx found, ttyd listening directly on :$TTYD_PORT"
    # Drop --port/internal + --interface/127.0.0.1 (indices 1-4),
    # keep --writable (0) and client options (5+).
    ttyd_args=(--writable --port "$TTYD_PORT" "${ttyd_args[@]:5}")
fi

log "Browser terminal: http://<this-server>:$TTYD_PORT"

MATRIX_STORE_DIR="${MATRIX_STORE_PATH:-/var/lib/party-line-pager/matrix}"
ENV_FILE="$CONFIG_DIR/.env"
export CONFIG_DIR STATE_DIR POLICY_FILE ADAPTERS_FILE MATRIX_STORE_DIR ENV_FILE
exec ttyd "${ttyd_args[@]}" bash /opt/party-line-pager/party-line-pager.sh
