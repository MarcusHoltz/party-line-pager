#!/usr/bin/env bash
#
# PartylinePager setup and administration menu.
#
# A front end over `docker compose` and `partylinepagerctl`. It writes .env,
# adapters.toml and policy.toml, gates the first start on the requirements
# actually being met, and wraps every admin command the daemon already has.
#
# Nothing here is required: every action maps onto a command documented in
# README.md, which stays the reference. This script only saves the typing.
#
# The config files are read once at startup and owned by this script while it
# runs. To hand-edit them, quit, edit, and start the script again.

if [ -z "${BASH_VERSINFO:-}" ] || [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
    echo "partylinepager.sh needs bash 4 or newer. Try: bash partylinepager.sh" >&2
    exit 1
fi

set -uo pipefail

# Everything is relative to wherever this file lives, so the project directory
# can be moved or renamed without editing anything.
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_DIR="${CONFIG_DIR:-$ROOT/config}"
ENV_FILE="${ENV_FILE:-$ROOT/.env}"
ADAPTERS_FILE="${ADAPTERS_FILE:-$CONFIG_DIR/adapters.toml}"
POLICY_FILE="${POLICY_FILE:-$CONFIG_DIR/policy.toml}"
STATE_DIR="${STATE_DIR:-$CONFIG_DIR/state}"
# The Matrix E2EE store, bind-mounted by compose/docker-compose.matrix.yml. Must exist
# before compose runs, or Docker creates it owned by root.
MATRIX_STORE_DIR="${MATRIX_STORE_DIR:-$CONFIG_DIR/matrix-store}"
# The three party lines. Same shape, one transport dir each, and each one gets
# its own directory variable in the compose overlay because all the hooks run in
# one partylinepagerd container: a shared TOR_PARTYLINE_DIR would have whichever
# overlay loaded last silently point every hook at one directory.
PARTY_LINES=(tor i2p rns)
declare -A PARTYLINE_DIRS=()
declare -A PARTYLINE_ENVVARS=(
    [tor]=TOR_PARTYLINE_DIR
    [i2p]=I2P_PARTYLINE_DIR
    [rns]=RETICULUM_PARTYLINE_DIR
)
declare -A PARTYLINE_NAMES=(
    [tor]=tor-party-line
    [i2p]=i2p-party-line
    [rns]=reticulum-party-line
)
_pl_dir() {
    local name="$1"
    local transport="$ROOT/transports/$name"
    local checkout="$ROOT/party-lines"
    if [[ -f "$transport/docker-compose.yml" ]]; then echo "$transport"
    elif [[ -d "$checkout/${name}-main" ]]; then echo "$checkout/${name}-main"
    elif [[ -d "$checkout/$name" ]]; then echo "$checkout/$name"
    else echo "$transport"
    fi
}
_resolve_pl_dirs() {
    local pl envvar name
    for pl in "${PARTY_LINES[@]}"; do
        envvar="${PARTYLINE_ENVVARS[$pl]}"
        name="${PARTYLINE_NAMES[$pl]}"
        if [[ -n "${ENVV[$envvar]:-}" ]]; then
            PARTYLINE_DIRS[$pl]="${ENVV[$envvar]}"
        else
            PARTYLINE_DIRS[$pl]="$(_pl_dir "$name")"
        fi
    done
}
declare -A PARTYLINE_REPOS=(
    [tor]="https://gitlab.com/MarcusHoltz/tor-party-line.git"
    [i2p]="https://gitlab.com/MarcusHoltz/i2p-party-line.git"
    [rns]="https://gitlab.com/MarcusHoltz/reticulum-party-line.git"
)
declare -A PROVIDER_LABELS=(
    [tor]="Tor party line"
    [i2p]="I2P party line"
    [rns]="Reticulum party line"
    [web]="Web rooms"
)
# /var/tmp, not /tmp. On a systemd host /tmp is usually a tmpfs sized at half
# of RAM: a backup taken there does not survive a reboot, and the tarball this
# script writes before a destructive action is large enough to compete with
# memory. /var/tmp is on disk and is the FHS location for exactly this.
BACKUP_ROOT="/var/tmp/BACKUP"

ALL_ADAPTERS=(telegram matrix irc xmpp mastodon email signal mattermost discord)
ALL_PROVIDERS=(tor i2p rns web)

# V1 (full image) vs V2 (docker compose). deploy/Dockerfile.full sets this to "direct".
PLP_RUNTIME="${PARTYLINEPAGER_RUNTIME:-compose}"
_is_direct() { [[ $PLP_RUNTIME == direct ]]; }
PLP_PID_DIR="${PLP_PID_DIR:-/var/run/plp}"
PLP_LOG_DIR="${PLP_PID_DIR}"

# --------------------------------------------------------------------------
# Output
# --------------------------------------------------------------------------

if [[ -t 1 ]]; then
    B=$'\033[1m'; DIM=$'\033[2m'
    RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; NC=$'\033[0m'
else
    B=""; DIM=""; RED=""; GRN=""; YEL=""; NC=""
fi

RULE="─────────────────────────────────────────────────"

title() { clear; printf '\n  %s%s%s\n  %s\n' "$B" "$1" "$NC" "$RULE"; }
say()   { printf '  %s\n' "$*"; }
note()  { printf '  %s%s%s\n' "$DIM" "$*" "$NC"; }
good()  { printf '  %s%s%s\n' "$GRN" "$*" "$NC"; }
warn()  { printf '  %s%s%s\n' "$YEL" "$*" "$NC"; }
oops()  { printf '  %s%s%s\n' "$RED" "$*" "$NC"; }
blank() { printf '\n'; }

# One menu line: key, label, and an optional value or hint.
#
#   item 1 "Service" "running"
#
# The label column is padded here rather than by hand at each call site. Hand
# padding is what the menus used to do, and it drifts the moment a label is
# reworded: every other line then has to be recounted to match. ANSI escapes
# are deliberately kept out of the padded field for the same reason, since
# printf pads by byte and would count the invisible escape toward the width.
# Colour goes in the value, after the padding is done.
ITEM_WIDTH=22
item() {
    if [[ -n ${3:-} ]]; then
        printf '   %s) %-*s %b\n' "$1" "$ITEM_WIDTH" "$2" "$3"
    else
        # No value means nothing to line up against, so no trailing padding.
        printf '   %s) %s\n' "$1" "$2"
    fi
}

# A plural that reads correctly at one.
#
#   count 1 subscriber   -> "1 subscriber"
#   count 3 subscriber   -> "3 subscribers"
count() { printf '%s %s%s' "$1" "$2" "$([[ $1 -eq 1 ]] || printf s)"; }

# A continuation line, indented to where an item's value column starts.
cont() { printf '   %*s %b%s%b\n' "$((ITEM_WIDTH + 3))" "" "$DIM" "$*" "$NC"; }

# policy.toml stores these as TOML booleans; a menu should not.
yesno() { [[ $1 == true ]] && printf yes || printf no; }

pause() { blank; read -rsn1 -p "  Press any key to go back. " _; blank; }

# ask VAR "prompt" ["default"]
ask() {
    local __var=$1 prompt=$2 def=${3:-} reply
    if [[ -n $def ]]; then
        read -rp "  $prompt [$def]: " reply
        reply=${reply:-$def}
    else
        read -rp "  $prompt: " reply
    fi
    printf -v "$__var" '%s' "$reply"
}

# Never echoes. Empty reply keeps whatever is already stored.
ask_secret() {
    local __var=$1 prompt=$2 reply
    read -rsp "  $prompt: " reply; blank
    printf -v "$__var" '%s' "$reply"
}

confirm() { local reply; read -rp "  $1 [y/N]: " reply; [[ ${reply,,} == y* ]]; }

trim() {
    local s=$1
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

unquote() { local s=$1; s="${s%\"}"; s="${s#\"}"; printf '%s' "$s"; }

have() { command -v "$1" >/dev/null 2>&1; }

# Copies into $BACKUP_ROOT keeping the absolute path, and says where it went.
backup_file() {
    local src=$1 dest="$BACKUP_ROOT$1"
    [[ -f $src ]] || return 0
    mkdir -p "$(dirname "$dest")" 2>/dev/null || { warn "Could not back up $src"; return 0; }
    if cp -p "$src" "$dest" 2>/dev/null; then
        note "Backed up: $dest"
    else
        warn "Could not back up $src"
    fi
}

# --------------------------------------------------------------------------
# Docker
# --------------------------------------------------------------------------

# The compose files this instance actually needs: the base daemon, one overlay
# per adapter switched on in adapters.toml, one per party line policy.toml asks
# for, and Apprise.
#
# Adapters that need no container of their own still have an overlay, because
# that overlay is what passes their secret into the daemon. Loading only the
# ones in use keeps every other credential out of the process environment, and
# a web-only instance out of the Docker socket.
#
# Apprise is unconditional here. An admin can add an `apprise:` endpoint with
# partylinepagerctl at any moment, and a sidecar that is not running turns that into
# a failed delivery with no warning. Drive `docker compose -f ...` by hand to
# leave it out; the README lists the files.
compose_files() {
    local a
    printf '%s\n' -f "$ROOT/docker-compose.yml"
    for a in "${ALL_ADAPTERS[@]}"; do
        adapter_on "$a" && [[ -f "$ROOT/compose/docker-compose.$a.yml" ]] \
            && printf '%s\n' -f "$ROOT/compose/docker-compose.$a.yml"
    done
    # The Docker socket is granted once, not once per party line: compose
    # concatenates group_add and refuses a list with two equal entries.
    # V1 (direct mode) does not need a docker socket.
    uses_any_partyline && ! _is_direct && printf '%s\n' -f "$ROOT/compose/docker-compose.docker-socket.yml"
    local pl
    for pl in "${PARTY_LINES[@]}"; do
        uses_provider "$pl" && [[ -f "$ROOT/compose/docker-compose.$pl.yml" ]] \
            && printf '%s\n' -f "$ROOT/compose/docker-compose.$pl.yml"
    done
    printf '%s\n' -f "$ROOT/compose/docker-compose.apprise.yml"
}

dc() {
    if _is_direct; then
        _dc_direct "$@"
    else
        local files=()
        mapfile -t files < <(compose_files)
        ( cd "$ROOT" && docker compose "${files[@]}" "$@" )
    fi
}

_daemon_pid_file() { echo "$PLP_PID_DIR/partylinepagerd.pid"; }
_daemon_log_file() { echo "$PLP_LOG_DIR/partylinepagerd.log"; }

_daemon_pid_alive() {
    local pf; pf="$(_daemon_pid_file)"
    [[ -f $pf ]] && kill -0 "$(cat "$pf")" 2>/dev/null
}

_dc_export_env() {
    [[ -f $ENV_FILE ]] || return 0
    local line key val
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line =~ ^[[:space:]]*# ]] && continue
        [[ $line != *=* ]] && continue
        key="${line%%=*}"; key="${key#"${key%%[![:space:]]*}"}"
        val="${line#*=}"
        [[ $key =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
        export "$key=$val"
    done < "$ENV_FILE"
}

_dc_direct() {
    case "$1" in
        build) ;;
        up)
            shift
            # In V1 only the daemon runs here; other services (signal-cli,
            # transports) are managed by the entrypoint or plp-runtime.sh.
            # Consume compose flags, then check if a specific service was
            # named. A bare `dc up -d` starts the daemon; a targeted
            # `dc up -d signal-cli-rest-api` is a no-op.
            local _up_svc=""
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    -d|--remove-orphans) shift ;;
                    -*) shift ;;
                    *) _up_svc="$1"; break ;;
                esac
            done
            if [[ -n $_up_svc && $_up_svc != partylinepagerd ]]; then
                return 0
            fi
            mkdir -p "$PLP_PID_DIR" "$STATE_DIR"
            adapter_on matrix && mkdir -p "$MATRIX_STORE_DIR"
            if _daemon_pid_alive; then
                echo "partylinepagerd already running (pid $(cat "$(_daemon_pid_file)"))"
                return 0
            fi
            # Source .env so env:NAME references in adapters.toml resolve.
            # In V2 docker compose does this; in V1 we do it by hand.
            _dc_export_env
            local lf; lf="$(_daemon_log_file)"
            partylinepagerd \
                --policy "$POLICY_FILE" \
                --adapters "$ADAPTERS_FILE" \
                --state "$STATE_DIR" \
                >>"$lf" 2>&1 &
            echo "$!" > "$(_daemon_pid_file)"
            ;;
        stop|down)
            local pf; pf="$(_daemon_pid_file)"
            if [[ -f $pf ]]; then
                kill "$(cat "$pf")" 2>/dev/null || true
                rm -f "$pf"
            fi
            ;;
        restart)
            _dc_direct stop
            sleep 1
            _dc_direct up -d
            ;;
        ps)
            if [[ "${2:-}" == "--services" ]]; then
                _daemon_pid_alive && echo "partylinepagerd"
            else
                if _daemon_pid_alive; then
                    echo "partylinepagerd  running  (pid $(cat "$(_daemon_pid_file)"))"
                else
                    echo "partylinepagerd  stopped"
                fi
            fi
            ;;
        run)
            shift
            local cmd=""
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    --rm|--no-deps) shift ;;
                    --entrypoint) shift; cmd="$1"; shift ;;
                    partylinepagerd) shift; break ;;
                    *) shift ;;
                esac
            done
            _dc_export_env
            if [[ -n $cmd ]]; then
                "$cmd" --state "$STATE_DIR" "$@"
            else
                partylinepagerd \
                    --policy "$POLICY_FILE" \
                    --adapters "$ADAPTERS_FILE" \
                    --state "$STATE_DIR" \
                    "$@"
            fi
            ;;
        exec)
            shift
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    -T) shift ;;
                    partylinepagerd) shift; break ;;
                    *) shift ;;
                esac
            done
            "$@"
            ;;
        logs)
            shift
            local lf; lf="$(_daemon_log_file)"
            local nlines=50
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    --tail) shift; nlines="${1:-50}"; shift ;;
                    -f)     shift ;;
                    *)      shift ;;
                esac
            done
            tail -n "$nlines" -f "$lf" 2>/dev/null
            ;;
        images)
            echo "built-in"
            ;;
        *)
            echo "dc (direct mode): unknown command: $1" >&2
            return 1
            ;;
    esac
}

svc_running() {
    if _is_direct; then
        _daemon_pid_alive
    else
        dc ps --services --status running 2>/dev/null | grep -qx partylinepagerd
    fi
}

image_built() {
    if _is_direct; then
        return 0
    fi
    [[ -n "$(dc images -q partylinepagerd 2>/dev/null)" ]]
}

ctl() {
    if _is_direct; then
        partylinepagerctl --state "$STATE_DIR" "$@"
        return
    fi
    if svc_running; then
        dc exec -T partylinepagerd partylinepagerctl "$@"
    else
        dc run --rm --no-deps --entrypoint partylinepagerctl partylinepagerd "$@" </dev/null
    fi
}

# Ctrl-C should stop following the logs, not kill this script.
follow_logs() {
    note "Ctrl-C stops following. The service keeps running."
    blank
    trap ':' INT
    dc logs -f --tail 50 "$@"
    trap - INT
    blank
}

# --------------------------------------------------------------------------
# .env
# --------------------------------------------------------------------------

declare -A ENVV

load_env() {
    ENVV=()
    [[ -f $ENV_FILE ]] || return 0
    local line key val
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line =~ ^[[:space:]]*# ]] && continue
        [[ $line != *=* ]] && continue
        key=$(trim "${line%%=*}")
        val=${line#*=}
        [[ $key =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
        ENVV[$key]=$val
    done < "$ENV_FILE"
}

save_env() {
    backup_file "$ENV_FILE"
    {
        echo "# Written by partylinepager.sh. Real credentials: keep this out of git."
        echo "# Referenced as env:NAME from adapters.toml, and passed to the daemon"
        echo "# by the compose/docker-compose.<network>.yml overlay for that network."
        echo
        echo "# Who the containers run as, so files under ./config/state belong to you."
        local k
        for k in HOST_UID HOST_GID DOCKER_GID; do
            [[ -n ${ENVV[$k]:-} ]] && echo "$k=${ENVV[$k]}"
        done
        echo
        echo "# The compose files this instance is made of, so that a bare"
        echo "# 'docker compose up -d' in this directory means what this script"
        echo "# means. Rewritten whenever an adapter is switched on or off."
        local f list=""
        while read -r f; do
            [[ $f == -f ]] && continue
            list+="${f#"$ROOT/"}:"
        done < <(compose_files)
        echo "COMPOSE_FILE=${list%:}"
        echo
        echo "# Adapter credentials."
        for k in $(printf '%s\n' "${!ENVV[@]}" | sort); do
            case $k in HOST_UID|HOST_GID|DOCKER_GID|COMPOSE_FILE) continue ;; esac
            echo "$k=${ENVV[$k]}"
        done
    } > "$ENV_FILE"
    chmod 600 "$ENV_FILE"
}

# --------------------------------------------------------------------------
# adapters.toml
# --------------------------------------------------------------------------

declare -A A   # A[section.key] = value

load_adapters() {
    A=()
    [[ -f $ADAPTERS_FILE ]] || return 0
    local line sect="" key val
    while IFS= read -r line || [[ -n $line ]]; do
        line=$(trim "$line")
        [[ -z $line || $line == \#* ]] && continue
        if [[ $line =~ ^\[([a-z_]+)\]$ ]]; then sect="${BASH_REMATCH[1]}"; continue; fi
        [[ -z $sect || $line != *=* ]] && continue
        key=$(trim "${line%%=*}")
        val=$(unquote "$(trim "${line#*=}")")
        A[$sect.$key]=$val
    done < "$ADAPTERS_FILE"

    # IRC's password is the one optional secret, so the writer needs to know
    # whether to emit its line. Derive that from the file rather than from a
    # flag that only exists in memory, or a reload followed by any save would
    # silently drop the password line.
    [[ -n ${A[irc.password]:-} ]] && A[irc.use_password]=true
    return 0
}

adapter_on() { [[ ${A[$1.enabled]:-false} == true ]]; }

enabled_adapters() {
    local a out=()
    for a in "${ALL_ADAPTERS[@]}"; do adapter_on "$a" && out+=("$a"); done
    (( ${#out[@]} )) && printf '%s' "$(IFS=,; echo "${out[*]}")"
}

save_adapters() {
    backup_file "$ADAPTERS_FILE"
    {
        echo "# PartylinePager chat network credentials. Written by partylinepager.sh."
        echo "# Secrets are env:NAME and live in .env. Full field reference:"
        echo "# adapters.example.toml and the adapters.toml section of README.md."

        if adapter_on telegram; then
            printf '\n[telegram]\nenabled = true\ntoken = "env:TELEGRAM_TOKEN"\n'
        fi
        # Every field takes a default. A section hand-edited to enabled = true
        # with a field missing should still produce a file the daemon can read
        # and reject clearly, not abort this script under set -u.
        if adapter_on matrix; then
            printf '\n[matrix]\nenabled = true\nhomeserver = "%s"\nuser = "%s"\npassword = "env:MATRIX_PASSWORD"\nstore_path = "%s"\n' \
                "${A[matrix.homeserver]:-}" "${A[matrix.user]:-}" "${A[matrix.store_path]:-/var/lib/partylinepager/matrix}"
        fi
        if adapter_on irc; then
            printf '\n[irc]\nenabled = true\nserver = "%s"\nport = %s\ntls = %s\nnick = "%s"\n' \
                "${A[irc.server]:-}" "${A[irc.port]:-6697}" "${A[irc.tls]:-true}" "${A[irc.nick]:-partylinepager}"
            [[ -n ${A[irc.account]:-} ]] && printf 'account = "%s"\n' "${A[irc.account]}"
            [[ ${A[irc.use_password]:-false} == true ]] && printf 'password = "env:IRC_PASSWORD"\n'
            [[ -n ${A[irc.channels]:-} ]] && printf 'channels = %s\n' "${A[irc.channels]}"
        fi
        if adapter_on xmpp; then
            printf '\n[xmpp]\nenabled = true\njid = "%s"\npassword = "env:XMPP_PASSWORD"\ntls = %s\n' \
                "${A[xmpp.jid]:-}" "${A[xmpp.tls]:-true}"
        fi
        if adapter_on mastodon; then
            printf '\n[mastodon]\nenabled = true\nbase_url = "%s"\naccess_token = "env:MASTODON_TOKEN"\npoll_interval = "%s"\n' \
                "${A[mastodon.base_url]:-}" "${A[mastodon.poll_interval]:-30s}"
        fi
        if adapter_on email; then
            printf '\n[email]\nenabled = true\nimap_host = "%s"\nimap_port = %s\nimap_user = "%s"\nimap_password = "env:IMAP_PASSWORD"\nmailbox = "%s"\n\nsmtp_host = "%s"\nsmtp_port = %s\nsmtp_user = "%s"\nsmtp_password = "env:SMTP_PASSWORD"\nfrom = "%s"\n\ntls = %s\npoll_interval = "%s"\n' \
                "${A[email.imap_host]:-}" "${A[email.imap_port]:-993}" "${A[email.imap_user]:-}" \
                "${A[email.mailbox]:-INBOX}" \
                "${A[email.smtp_host]:-}" "${A[email.smtp_port]:-587}" "${A[email.smtp_user]:-}" \
                "${A[email.from]:-}" "${A[email.tls]:-true}" "${A[email.poll_interval]:-60s}"
        fi
        if adapter_on signal; then
            printf '\n[signal]\nenabled = true\nrest_url = "%s"\nnumber = "%s"\n' \
                "${A[signal.rest_url]:-http://signal-cli-rest-api:8080}" \
                "${A[signal.number]:-}"
        fi
        if adapter_on mattermost; then
            printf '\n[mattermost]\nenabled = true\nbase_url = "%s"\naccess_token = "env:MATTERMOST_TOKEN"\n' \
                "${A[mattermost.base_url]:-}"
        fi
        if adapter_on discord; then
            printf '\n[discord]\nenabled = true\ntoken = "env:DISCORD_TOKEN"\n'
        fi
    } > "$ADAPTERS_FILE"
    chmod 600 "$ADAPTERS_FILE"
}

# --------------------------------------------------------------------------
# policy.toml
# --------------------------------------------------------------------------

declare -A P
declare -a TIER_NAME TIER_WINDOW TIER_MAXROOMS TIER_OPEN TIER_RECV TIER_HOLD TIER_CLOSE

policy_defaults() {
    P=(
        [name]="PartylinePager" [signups]="approval" [default_tier]="weekly"
        [room_ttl]="2h" [hook_timeout]="300s" [paused]="false"
        [creds_delivery]="inline"
        [yopass_url]="https://share.yopass.se" [yopass_api]="https://api2.yopass.se"
        [apprise_url]="http://apprise:8000/notify"
        [providers]="tor web" [web_base_url]="https://p2p.mirotalk.com"
        [web_path]="join" [web_static_slug]=""
    )
    TIER_NAME=(lurker weekly daily trusted)
    TIER_WINDOW=("" "168h" "24h" "0s")
    TIER_MAXROOMS=(1 1 1 1)
    TIER_OPEN=(false true true true)
    TIER_RECV=(true true true true)
    TIER_HOLD=(false true false false)
    TIER_CLOSE=(true true true true)
}

load_policy() {
    policy_defaults
    [[ -f $POLICY_FILE ]] || return 0

    local line sect="" key val ti=-1
    local found_providers=()
    TIER_NAME=(); TIER_WINDOW=(); TIER_MAXROOMS=(); TIER_OPEN=(); TIER_RECV=(); TIER_HOLD=(); TIER_CLOSE=()

    while IFS= read -r line || [[ -n $line ]]; do
        line=$(trim "$line")
        [[ -z $line || $line == \#* ]] && continue

        if [[ $line =~ ^\[\[([a-z_]+)\]\]$ ]]; then
            sect="[[${BASH_REMATCH[1]}]]"
            if [[ $sect == "[[tier]]" ]]; then
                ti=$((ti + 1))
                TIER_NAME[ti]=""; TIER_WINDOW[ti]=""; TIER_MAXROOMS[ti]=1
                TIER_OPEN[ti]=true; TIER_RECV[ti]=true; TIER_HOLD[ti]=false; TIER_CLOSE[ti]=true
            fi
            continue
        fi
        # Digits included: [provider.i2p] is the only section name with one,
        # and without them it parses as no section at all and is silently lost.
        if [[ $line =~ ^\[([a-z0-9_.]+)\]$ ]]; then
            sect="${BASH_REMATCH[1]}"
            case "$sect" in
                provider.tor|provider.i2p|provider.rns|provider.web)
                    found_providers+=("${sect#provider.}") ;;
            esac
            continue
        fi
        [[ $line != *=* ]] && continue
        key=$(trim "${line%%=*}")
        val=$(unquote "$(trim "${line#*=}")")

        case "$sect" in
            instance)
                case $key in
                    name|signups|default_tier|room_ttl|hook_timeout|paused|creds_delivery|yopass_url|yopass_api) P[$key]=$val ;;
                esac ;;
            fanout)
                [[ $key == apprise_url ]] && P[apprise_url]=$val ;;
            provider.web)
                case $key in
                    base_url)    P[web_base_url]=$val ;;
                    path)        P[web_path]=$val ;;
                    static_slug) P[web_static_slug]=$val ;;
                esac ;;
            "[[tier]]")
                (( ti < 0 )) && continue
                case $key in
                    name)              TIER_NAME[ti]=$val ;;
                    window)            TIER_WINDOW[ti]=$val ;;
                    max_rooms)        TIER_MAXROOMS[ti]=$val ;;
                    may_open)         TIER_OPEN[ti]=$val ;;
                    may_receive)       TIER_RECV[ti]=$val ;;
                    hold_for_approval) TIER_HOLD[ti]=$val ;;
                    may_close)         TIER_CLOSE[ti]=$val ;;
                esac ;;
        esac
    done < "$POLICY_FILE"

    (( ${#found_providers[@]} )) && P[providers]="${found_providers[*]}"
    (( ${#TIER_NAME[@]} == 0 )) && { local keep=${P[providers]}; policy_defaults; P[providers]=$keep; }
}

# Tolerate being called before load_policy: compose_files needs this, and the
# menu is not the only entry point.

# Is one named provider switched on? Matched with spaces on both sides so
# "tor" never matches inside another name.
uses_provider() { [[ " ${P[providers]:-tor web} " == *" $1 "* ]]; }

# Is ANY party line switched on? What the Docker socket, the docker group and
# the privileged-hook warnings actually depend on; which network it is makes no
# difference to those.
uses_any_partyline() {
    local pl
    for pl in "${PARTY_LINES[@]}"; do uses_provider "$pl" && return 0; done
    return 1
}

uses_web() { uses_provider web; }

# Switch one provider on or off, keeping P[providers] in ALL_PROVIDERS order so
# policy.toml and the menu never disagree about ordering.
set_provider() {
    local want=$1 on=$2 p out=()
    for p in "${ALL_PROVIDERS[@]}"; do
        if [[ $p == "$want" ]]; then
            [[ $on == true ]] && out+=("$p")
        else
            uses_provider "$p" && out+=("$p")
        fi
    done
    P[providers]="${out[*]}"
}

save_policy() {
    backup_file "$POLICY_FILE"
    {
        echo "# PartylinePager policy. Written by partylinepager.sh. No secrets here."
        echo "# Read at startup only. Full reference: policy.example.toml and"
        echo "# the policy.toml section of README.md."
        echo
        echo "[instance]"
        printf 'name = "%s"\n'          "${P[name]}"
        printf 'signups = "%s"\n'       "${P[signups]}"
        printf 'default_tier = "%s"\n'  "${P[default_tier]}"
        printf 'room_ttl = "%s"\n'      "${P[room_ttl]}"
        printf 'hook_timeout = "%s"\n'  "${P[hook_timeout]}"
        printf 'paused = %s\n'          "${P[paused]}"
        printf 'creds_delivery = "%s"\n' "${P[creds_delivery]}"
        printf 'yopass_url = "%s"\n'     "${P[yopass_url]}"
        printf 'yopass_api = "%s"\n'     "${P[yopass_api]}"
        echo
        echo "[fanout]"
        printf 'apprise_url = "%s"\n'   "${P[apprise_url]}"
        printf 'timeout = "30s"\nconcurrency = 8\n'

        local pl
        for pl in "${PARTY_LINES[@]}"; do
            uses_provider "$pl" || continue
            echo
            echo "[provider.$pl]"
            printf 'up = "/opt/partylinepager/hooks/provider-%s.sh"\n' "$pl"
            printf 'down = "/opt/partylinepager/hooks/teardown-%s.sh"\n' "$pl"
        done
        if uses_web; then
            echo
            echo "[provider.web]"
            echo 'up = "/opt/partylinepager/hooks/provider-web.sh"'
            echo 'down = "/opt/partylinepager/hooks/teardown-web.sh"'
            printf 'base_url = "%s"\n' "${P[web_base_url]}"
            printf 'path = "%s"\n' "${P[web_path]}"
            [[ -n ${P[web_static_slug]:-} ]] && printf 'static_slug = "%s"\n' "${P[web_static_slug]}"
        fi

        local i
        for i in "${!TIER_NAME[@]}"; do
            [[ -z ${TIER_NAME[i]} ]] && continue
            echo
            echo "[[tier]]"
            printf 'name = "%s"\n' "${TIER_NAME[i]}"
            [[ -n ${TIER_WINDOW[i]} ]]      && printf 'window = "%s"\n' "${TIER_WINDOW[i]}"
            [[ ${TIER_MAXROOMS[i]:-1} != 1 ]] && printf 'max_rooms = %s\n' "${TIER_MAXROOMS[i]}"
            [[ ${TIER_OPEN[i]} == false ]] && echo 'may_open = false'
            [[ ${TIER_RECV[i]}  == false ]] && echo 'may_receive = false'
            [[ ${TIER_HOLD[i]}  == true  ]] && echo 'hold_for_approval = true'
            [[ ${TIER_CLOSE[i]} == false ]] && echo 'may_close = false'
        done
    } > "$POLICY_FILE"

    # policy.toml is read at startup only; a running daemon would otherwise
    # keep serving the pre-edit policy until someone remembered to restart it
    # by hand.
    if svc_running; then
        dc restart partylinepagerd >/dev/null 2>&1
        good "partylinepagerd restarted to pick up the change."
    fi
}

# --------------------------------------------------------------------------
# Live status
# --------------------------------------------------------------------------

sub_count() {
    [[ -f $STATE_DIR/subscribers.json ]] || { echo 0; return; }
    # grep -c prints 0 and exits 1 on no match, so take the output, not the code.
    local n; n=$(grep -c '"endpoint"' "$STATE_DIR/subscribers.json" 2>/dev/null)
    echo "${n:-0}"
}

is_paused() {
    [[ -f $STATE_DIR/runtime.json ]] && grep -q '"paused": *true' "$STATE_DIR/runtime.json"
}

configured() { [[ -f $ADAPTERS_FILE && -f $POLICY_FILE && -n "$(enabled_adapters)" ]]; }

# --------------------------------------------------------------------------
# System check
# --------------------------------------------------------------------------

CHECK_FAILS=0

check_item() {  # check_item "label" "fix hint" <test result 0/1>
    if (( $3 == 0 )); then
        printf '  %s%-32s ok%s\n' "$GRN" "$1" "$NC"
    else
        printf '  %s%-32s MISSING%s\n' "$RED" "$1" "$NC"
        note "    $2"
        CHECK_FAILS=$((CHECK_FAILS + 1))
    fi
}

run_system_check() {
    CHECK_FAILS=0
    local rc

    if _is_direct; then
        good "  V1 (full image) mode, docker checks skipped"
    else
        have docker; rc=$?
        check_item "docker" "Install Docker Engine, then log out and back in." $rc

        if (( rc == 0 )); then
            docker compose version >/dev/null 2>&1; rc=$?
            check_item "docker compose v2 plugin" \
                "The old standalone docker-compose is not enough. Install the compose plugin." $rc

            docker info >/dev/null 2>&1; rc=$?
            check_item "docker daemon reachable" \
                "Start it (systemctl start docker) and add yourself: sudo usermod -aG docker \$USER" $rc
        fi
    fi

    [[ -w $ROOT ]]; rc=$?
    check_item "project directory writable" "chown this directory to your user." $rc

    local root_dir free_kb=0
    root_dir=$(docker info --format '{{.DockerRootDir}}' 2>/dev/null)
    [[ -z $root_dir ]] && root_dir=$ROOT
    free_kb=$(df -Pk "$root_dir" 2>/dev/null | awk 'NR==2 {print $4}')
    free_kb=${free_kb:-0}
    if (( free_kb < 3145728 )); then
        check_item "disk space for the build" \
            "Under 3G free on $root_dir. The release build needs several gigabytes." 1
    else
        printf '  %s%-32s ok%s  %s free on %s\n' "$GRN" "disk space for the build" "$NC" \
            "$(( free_kb / 1048576 ))G" "$root_dir"
        (( free_kb < 8388608 )) && note "    Under 8G free. Tight but usually enough."
    fi

    if uses_any_partyline; then
        local pl dir needs_git=false
        for pl in "${PARTY_LINES[@]}"; do
            uses_provider "$pl" || continue
            dir="${PARTYLINE_DIRS[$pl]}"
            [[ $dir == */party-lines/* ]] && needs_git=true
            [[ -d $dir ]]; rc=$?
            check_item "./${dir#"$ROOT/"}" \
                "Set ${PARTYLINE_ENVVARS[$pl]} in .env, or: git clone ${PARTYLINE_REPOS[$pl]}" $rc
        done
        if $needs_git; then
            have git; rc=$?
            check_item "git (for party line checkouts)" "Install git, or switch to image mode." $rc
        fi
    fi

    blank
    note "Optional, for showing the Signal link QR in this terminal:"
    local viewer="" vw
    for vw in chafa viu timg; do have "$vw" && { viewer=$vw; break; }; done
    if [[ -n $viewer ]]; then
        good "  $viewer found"
    else
        note "  none found. Install chafa for an in-terminal QR; without it the"
        note "  QR is saved as a PNG you open yourself."
    fi
}

system_check_menu() {
    title "System check"
    blank
    run_system_check
    blank
    if (( CHECK_FAILS == 0 )); then
        good "Everything required is present."
    else
        oops "$CHECK_FAILS requirement(s) missing. Fix them, then check again."
    fi
    pause
}

# --------------------------------------------------------------------------
# Host identity
# --------------------------------------------------------------------------

identity_state() {
    if [[ -n ${ENVV[HOST_UID]:-} && -n ${ENVV[HOST_GID]:-} ]]; then
        printf 'uid %s, gid %s' "${ENVV[HOST_UID]}" "${ENVV[HOST_GID]}"
        [[ -n ${ENVV[DOCKER_GID]:-} ]] && printf ', docker gid %s' "${ENVV[DOCKER_GID]}"
    else
        printf 'not set'
    fi
}

identity_menu() {
    title "Host identity"
    blank
    say "The containers write into ./config/state. Running them as you rather than"
    say "root keeps the whole directory movable without sudo."
    blank

    local uid gid dgid
    uid=$(id -u); gid=$(id -g)
    dgid=$(getent group docker 2>/dev/null | cut -d: -f3)

    say "Detected: uid $uid, gid $gid, docker group ${dgid:-none}"
    blank

    if [[ -z $dgid ]] && uses_any_partyline && ! _is_direct; then
        warn "No docker group on this host. The party line hooks need one to"
        warn "reach the Docker socket."
        blank
    fi

    if confirm "Use these?"; then
        ENVV[HOST_UID]=$uid
        ENVV[HOST_GID]=$gid
        [[ -n $dgid ]] && ENVV[DOCKER_GID]=$dgid
        save_env
        good "Saved to .env"
    fi
    pause
}

# --------------------------------------------------------------------------
# Adapters
# --------------------------------------------------------------------------

# set_secret ENV_NAME "prompt"  - keeps the existing value on an empty reply
set_secret() {
    local key=$1 prompt=$2 val
    if [[ -n ${ENVV[$key]:-} ]]; then
        ask_secret val "$prompt (enter keeps the current one)"
        [[ -n $val ]] && ENVV[$key]=$val
    else
        ask_secret val "$prompt"
        ENVV[$key]=$val
    fi
}

secret_state() { [[ -n ${ENVV[$1]:-} ]] && echo "set" || echo "${RED}not set${NC}"; }

configure_telegram() {
    title "Telegram"
    blank
    note "Message @BotFather on Telegram, /newbot, and copy the token it gives you."
    note "Subscribers then talk to your bot in a private chat."
    blank
    say "Token: $(secret_state TELEGRAM_TOKEN)"
    blank
    set_secret TELEGRAM_TOKEN "Bot token"
    A[telegram.enabled]=true
}

configure_matrix() {
    title "Matrix"
    blank
    note "End-to-end encrypted, the best of the seven for carrying a secret."
    note "Register an account for the bot on your homeserver first."
    blank
    ask v "Homeserver URL" "${A[matrix.homeserver]:-https://matrix.example.org}"; A[matrix.homeserver]=$v
    ask v "Username (localpart, not the full @user:server)" "${A[matrix.user]:-partylinepager}"; A[matrix.user]=$v
    say "Password: $(secret_state MATRIX_PASSWORD)"
    set_secret MATRIX_PASSWORD "Password"
    A[matrix.store_path]="/var/lib/partylinepager/matrix"
    A[matrix.enabled]=true
}

configure_irc() {
    title "IRC"
    blank
    warn "IRC has no offline delivery. A broadcast to somebody who is not"
    warn "connected is simply lost. Tell IRC-only subscribers to add a second"
    warn "endpoint."
    blank
    ask v "Server" "${A[irc.server]:-irc.libera.chat}"; A[irc.server]=$v
    ask v "Port" "${A[irc.port]:-6697}"; A[irc.port]=$v
    ask v "TLS (true/false)" "${A[irc.tls]:-true}"; A[irc.tls]=$v
    ask v "Nick" "${A[irc.nick]:-partylinepager}"; A[irc.nick]=$v
    blank
    note "Register the nick with NickServ before you enable this. Networks that"
    note "reserve registered nicks refuse or rename a bot that cannot log in,"
    note "and a bot under the wrong nick receives nothing."
    note "On Libera: /msg NickServ REGISTER <password> <email>"
    blank
    ask v "Services account, blank when it matches the nick" "${A[irc.account]:-}"
    A[irc.account]=$(trim "$v")
    if confirm "Log in to services with a password?"; then
        set_secret IRC_PASSWORD "services account password"
        A[irc.use_password]=true
    else
        A[irc.use_password]=false
    fi
    blank
    note "Channels only decide where the bot idles so people can find it."
    note "Channel traffic is never treated as a command. Blank for none."
    ask v "Channels, comma separated" ""
    if [[ -n $v ]]; then
        local out="" c
        IFS=',' read -ra chans <<< "$v"
        for c in "${chans[@]}"; do c=$(trim "$c"); [[ -n $c ]] && out+="\"$c\", "; done
        A[irc.channels]="[${out%, }]"
    else
        A[irc.channels]=""
    fi
    A[irc.enabled]=true
}

configure_xmpp() {
    title "XMPP"
    blank
    note "A bare JID, not a full one: the resource is chosen at connect time."
    blank
    ask v "JID" "${A[xmpp.jid]:-partylinepager@example.org}"; A[xmpp.jid]=$v
    say "Password: $(secret_state XMPP_PASSWORD)"
    set_secret XMPP_PASSWORD "Password"
    blank
    note "TLS false sends the password over the wire in the clear. It exists"
    note "for the throwaway server the live tests use. Leave it true."
    ask v "TLS (true/false)" "${A[xmpp.tls]:-true}"; A[xmpp.tls]=$v
    A[xmpp.enabled]=true
}

configure_mastodon() {
    title "Mastodon"
    blank
    note "Settings > Development > New application. It needs read:notifications"
    note "and write:statuses. Only direct-visibility mentions count as commands,"
    note "and every reply goes out direct."
    blank
    ask v "Instance URL" "${A[mastodon.base_url]:-https://mastodon.example.org}"; A[mastodon.base_url]=$v
    say "Access token: $(secret_state MASTODON_TOKEN)"
    set_secret MASTODON_TOKEN "Access token"
    ask v "Poll interval" "${A[mastodon.poll_interval]:-30s}"; A[mastodon.poll_interval]=$v
    A[mastodon.enabled]=true
}

configure_email() {
    title "Email"
    blank
    note "The universal fallback: anybody on any service can drive the bot from"
    note "a mail client. Unseen messages are read and then marked seen, so give"
    note "the bot its own mailbox."
    blank
    ask v "IMAP host" "${A[email.imap_host]:-imap.example.org}"; A[email.imap_host]=$v
    ask v "IMAP port" "${A[email.imap_port]:-993}"; A[email.imap_port]=$v
    ask v "IMAP user" "${A[email.imap_user]:-partylinepager@example.org}"; A[email.imap_user]=$v
    set_secret IMAP_PASSWORD "IMAP password"
    ask v "Mailbox" "${A[email.mailbox]:-INBOX}"; A[email.mailbox]=$v
    blank
    ask v "SMTP host" "${A[email.smtp_host]:-smtp.example.org}"; A[email.smtp_host]=$v
    ask v "SMTP port" "${A[email.smtp_port]:-587}"; A[email.smtp_port]=$v
    ask v "SMTP user" "${A[email.smtp_user]:-${A[email.imap_user]}}"; A[email.smtp_user]=$v
    set_secret SMTP_PASSWORD "SMTP password"
    ask v "From address" "${A[email.from]:-${A[email.imap_user]}}"; A[email.from]=$v
    blank
    note "TLS false sends the mailbox password over the wire in the clear, on"
    note "both legs. It exists for the throwaway server the live tests use."
    note "Leave it true."
    ask v "TLS (true/false)" "${A[email.tls]:-true}"; A[email.tls]=$v
    ask v "Poll interval" "${A[email.poll_interval]:-60s}"; A[email.poll_interval]=$v
    A[email.enabled]=true
}

configure_signal() {
    title "Signal"
    blank
    note "Runs through the signal-cli-rest-api container in compose/docker-compose.signal.yml,"
    note "which this script loads whenever [signal] is switched on."
    note "After the build, use the Signal device link menu item to link a phone."
    blank
    ask v "Bot's phone number, with country code" "${A[signal.number]:-+15555550100}"; A[signal.number]=$v
    A[signal.rest_url]="http://signal-cli-rest-api:8080"
    A[signal.enabled]=true
}

configure_mattermost() {
    title "Mattermost"
    blank
    note "Self-hosted, so there is no roster leak and no platform policy risk"
    note "the way there is for Discord. System Console > Integrations > turn on"
    note "personal access tokens, then create one for a bot account with"
    note "create_user_access_token. Only direct posts count as commands."
    blank
    ask v "Server URL" "${A[mattermost.base_url]:-https://mattermost.example.org}"; A[mattermost.base_url]=$v
    say "Access token: $(secret_state MATTERMOST_TOKEN)"
    set_secret MATTERMOST_TOKEN "Access token"
    A[mattermost.enabled]=true
}

configure_discord() {
    title "Discord"
    blank
    note "A bot may only DM someone it shares a guild with, so subscribers join"
    note "a lobby server first, then DM the bot. See the README's 'Setting up"
    note "Discord' section for the Developer Portal walkthrough and the lobby"
    note "server that keeps that mutual guild from leaking who is subscribed."
    blank
    say "Bot token: $(secret_state DISCORD_TOKEN)"
    set_secret DISCORD_TOKEN "Bot token"
    A[discord.enabled]=true
}

adapters_menu() {
    while true; do
        title "Adapters"
        blank
        say "Pick a network to configure. Configure at least one."
        blank
        local i=1 a state
        for a in "${ALL_ADAPTERS[@]}"; do
            if adapter_on "$a"; then state="${GRN}on${NC}"; else state="${DIM}off${NC}"; fi
            printf '   %d) %-10s %b\n' "$i" "$a" "$state"
            i=$((i + 1))
        done
        blank
        say "   d) Turn one off"
        item b "Back"
        blank
        note "Anything without an adapter can still be reached with apprise:"
        note "endpoints from the Subscribers menu."
        blank
        local choice; ask choice "Choose"

        case $choice in
            1) v=""; configure_telegram ;;
            2) v=""; configure_matrix ;;
            3) v=""; configure_irc ;;
            4) v=""; configure_xmpp ;;
            5) v=""; configure_mastodon ;;
            6) v=""; configure_email ;;
            7) v=""; configure_signal ;;
            8) v=""; configure_mattermost ;;
            9) v=""; configure_discord ;;
            d|D)
                local off; ask off "Which adapter to turn off"
                if [[ -n ${A[$off.enabled]:-} ]]; then
                    A[$off.enabled]=false
                    save_adapters
                    save_env
                    good "$off is off. Its credentials stay in .env."
                else
                    oops "No adapter called ${off:-that}."
                fi
                pause; continue ;;
            b|B|"") return ;;
            *) continue ;;
        esac

        save_adapters
        save_env
        blank
        good "Saved. adapters.toml and .env are mode 600."
        pause
    done
}

# --------------------------------------------------------------------------
# Provider and policy
# --------------------------------------------------------------------------

# One line per provider, each toggled on or off. A set rather than a choice,
# because an instance can offer any mix: all four, or only Reticulum, or Tor
# and web and nothing else.
providers_toggle_menu() {
    while true; do
        title "Which providers"
        blank
        say "Every provider switched on here becomes a command subscribers can type."
        say "At least one has to stay on, or nothing could ever be opened."
        blank
        local i=1 p state
        local -a order=()
        for p in "${ALL_PROVIDERS[@]}"; do
            if uses_provider "$p"; then state="on"; else state="off"; fi
            item "$i" "${PROVIDER_LABELS[$p]}" "$state"
            case $p in
                tor) cont "onion address plus a shared secret. Strongest" ;;
                i2p) cont "b32.i2p address plus a shared secret. Comes up in" ;;
                rns) cont "32-character hash plus a shared secret. Fastest of" ;;
                web) cont "a plain video-call link. Nothing is provisioned and" ;;
            esac
            case $p in
                tor) cont "anonymity, and the slowest to come up." ;;
                i2p) cont "about 23s against Tor's one to three minutes." ;;
                rns) cont "the three and needs no open port, but it is NOT" ;;
                web) cont "there is no secret: the unguessable URL is the" ;;
            esac
            case $p in
                rns) cont "onion routing. The relay sees each caller's IP." ;;
                web) cont "access control. No Docker socket needed." ;;
            esac
            order+=("$p")
            i=$((i + 1))
            blank
        done
        item b "Done"
        blank
        local c; ask c "Toggle which"
        case $c in
            b|B|"") break ;;
            [0-9]*)
                (( c >= 1 && c <= ${#order[@]} )) || continue
                p="${order[c-1]}"
                if uses_provider "$p"; then
                    # Refuse to leave the instance with no way to open a room
                    # anything: the daemon would not start.
                    local remaining=0 q
                    for q in "${ALL_PROVIDERS[@]}"; do
                        [[ $q == "$p" ]] && continue
                        uses_provider "$q" && remaining=$((remaining + 1))
                    done
                    if (( remaining == 0 )); then
                        blank
                        warn "That is the only provider left. Switch another on first."
                        pause
                        continue
                    fi
                    set_provider "$p" false
                else
                    set_provider "$p" true
                    clone_party_line "$p"
                fi
                save_policy
                # Which providers are on decides which compose overlays this
                # instance loads, so the COMPOSE_FILE line in .env has to be
                # rewritten too.
                save_env
                good "Saved to policy.toml"
                ;;
        esac
    done
}

# Offer to fetch a checkout when image mode is not available. A no-op for
# `web` (provisions nothing) and when the transport compose file exists
# (image mode is the primary path).
clone_party_line() {
    local pl=$1 name dir repo
    name="${PARTYLINE_NAMES[$pl]}"
    [[ -n $name ]] || return 0
    [[ -f "$ROOT/transports/$name/docker-compose.yml" ]] && return 0
    dir="${PARTYLINE_DIRS[$pl]:-}"
    [[ -n $dir ]] || return 0
    [[ -d $dir ]] && return 0
    repo="${PARTYLINE_REPOS[$pl]}"
    blank
    warn "./${dir#"$ROOT/"} is not checked out and no transport image is available."
    if confirm "Clone it now?"; then
        local target="$ROOT/party-lines/$name"
        git clone "$repo" "$target" && good "Cloned." || oops "Clone failed."
        PARTYLINE_DIRS[$pl]="$target"
    fi
}

providers_menu() {
    while true; do
        title "Providers"
        blank
        say "What opening a room actually provisions."
        blank
        item 1 "Which providers" "${P[providers]}"
        cont "any mix of tor, i2p, rns, web"
        if uses_web; then
            blank
            item 2 "Web base URL" "${P[web_base_url]}"
            item 3 "Web path" "${P[web_path]}"
            item 4 "Web room name" "${P[web_static_slug]:-random each room}"
        fi
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1)
                providers_toggle_menu
                pause ;;
            2)
                uses_web || continue
                blank
                note "The default MiroTalk instance is the public one, which means the"
                note "room list, the TURN servers and the logs are somebody else's."
                note "Point this at your own MiroTalk, Jitsi, or anything else that"
                note "hands out a room by URL."
                ask v "Web base URL" "${P[web_base_url]}"; P[web_base_url]=$v
                save_policy
                good "Saved to policy.toml"
                pause ;;
            3)
                uses_web || continue
                blank
                note "Segment(s) between the base URL and the room name, e.g. \"join\""
                note "for MiroTalk's /join/<room>. Leave blank for a backend that puts"
                note "rooms straight under the domain, like a self-hosted Jitsi's /<room>."
                ask v "Path" "${P[web_path]}"; P[web_path]=$v
                save_policy
                good "Saved to policy.toml"
                pause ;;
            4)
                uses_web || continue
                blank
                note "A room name here is reused on every room, so the link never"
                note "changes and regulars can join without waiting for a fresh one."
                note "Same access control as a random link either way: anyone who has"
                note "ever seen it can join. Leave blank for a fresh crypto-random"
                note "room name each time."
                ask v "Static room name (blank = random each room)" "${P[web_static_slug]}"
                P[web_static_slug]=$v
                save_policy
                good "Saved to policy.toml"
                pause ;;
            b|B|"") return ;;
        esac
    done
}

tiers_menu() {
    while true; do
        title "Tiers"
        blank
        note "Matched by name against each subscriber's tier. window is a rolling"
        note "quota: max_rooms rooms per window since the oldest one still in"
        note "it. Not calendar-aligned, so there is no thundering herd every"
        note "Monday at 00:00."
        blank
        local i w
        printf '       %s%-10s %-9s %6s  %-5s %-5s %s%s\n' \
            "$DIM" NAME WINDOW ROOMS OPEN HOLD CLOSE "$NC"
        for i in "${!TIER_NAME[@]}"; do
            # An absent or 0s window both mean no limit at all, so say that
            # rather than showing a blank that reads like "no quota configured".
            w=${TIER_WINDOW[i]}
            [[ -z $w || $w == 0s ]] && w="unlimited"
            printf '   %d)  %-10s %-9s %6s  %-5s %-5s %s\n' \
                "$((i + 1))" "${TIER_NAME[i]}" "$w" \
                "${TIER_MAXROOMS[i]:-1}" \
                "$(yesno "${TIER_OPEN[i]}")" \
                "$(yesno "${TIER_HOLD[i]}")" \
                "$(yesno "${TIER_CLOSE[i]}")"
        done
        blank
        item a "Add a tier"
        item d "Delete a tier"
        item b "Back"
        blank
        local c; ask c "Choose"

        case $c in
            b|B|"") save_policy; return ;;
            a|A)
                local n=${#TIER_NAME[@]}
                ask v "Name" ""; [[ -z $v ]] && continue
                TIER_NAME[n]=$v
                ask v "Window (e.g. 24h, 168h; 0s means unlimited)" "24h"; TIER_WINDOW[n]=$v
                if [[ -n $v && $v != 0s ]]; then
                    ask v "Max rooms per window" "1"; TIER_MAXROOMS[n]=$v
                else
                    TIER_MAXROOMS[n]=1
                fi
                confirm "May open a room?" && TIER_OPEN[n]=true || TIER_OPEN[n]=false
                confirm "May receive broadcasts?" && TIER_RECV[n]=true || TIER_RECV[n]=false
                confirm "Hold their requests for approval?" && TIER_HOLD[n]=true || TIER_HOLD[n]=false
                confirm "May close a room they opened, early?" && TIER_CLOSE[n]=true || TIER_CLOSE[n]=false
                save_policy ;;
            d|D)
                ask v "Number to delete" ""
                [[ $v =~ ^[0-9]+$ ]] || continue
                local x=$((v - 1))
                [[ -z ${TIER_NAME[x]:-} ]] && continue
                if (( ${#TIER_NAME[@]} <= 1 )); then oops "Keep at least one tier."; pause; continue; fi
                unset 'TIER_NAME[x]' 'TIER_WINDOW[x]' 'TIER_MAXROOMS[x]' 'TIER_OPEN[x]' 'TIER_RECV[x]' 'TIER_HOLD[x]' 'TIER_CLOSE[x]'
                TIER_NAME=("${TIER_NAME[@]}"); TIER_WINDOW=("${TIER_WINDOW[@]}"); TIER_MAXROOMS=("${TIER_MAXROOMS[@]}")
                TIER_OPEN=("${TIER_OPEN[@]}"); TIER_RECV=("${TIER_RECV[@]}"); TIER_HOLD=("${TIER_HOLD[@]}"); TIER_CLOSE=("${TIER_CLOSE[@]}")
                save_policy ;;
            [0-9]*)
                local x=$((c - 1))
                [[ -z ${TIER_NAME[x]:-} ]] && continue
                ask v "Name" "${TIER_NAME[x]}"; TIER_NAME[x]=$v
                ask v "Window (0s or blank means unlimited)" "${TIER_WINDOW[x]}"; TIER_WINDOW[x]=$v
                if [[ -n $v && $v != 0s ]]; then
                    ask v "Max rooms per window" "${TIER_MAXROOMS[x]:-1}"; TIER_MAXROOMS[x]=$v
                else
                    TIER_MAXROOMS[x]=1
                fi
                confirm "May open a room?" && TIER_OPEN[x]=true || TIER_OPEN[x]=false
                confirm "May receive broadcasts?" && TIER_RECV[x]=true || TIER_RECV[x]=false
                confirm "Hold their requests for approval?" && TIER_HOLD[x]=true || TIER_HOLD[x]=false
                confirm "May close a room they opened, early?" && TIER_CLOSE[x]=true || TIER_CLOSE[x]=false
                save_policy ;;
        esac
    done
}

policy_menu() {
    while true; do
        title "Policy"
        blank
        printf '   1) Instance name        %s\n' "${P[name]}"
        printf '   2) Signups              %s\n' "${P[signups]}"
        printf '   3) Default tier         %s\n' "${P[default_tier]}"
        printf '   4) Room lifetime        %s\n' "${P[room_ttl]}"
        printf '   5) Providers            %s\n' "${P[providers]}"
        printf '   6) Tiers                %d configured\n' "${#TIER_NAME[@]}"
        printf '   7) Credential delivery  %s\n' "${P[creds_delivery]}"
        if [[ ${P[creds_delivery]} == link ]]; then
            printf '   8) Yopass URL           %s\n' "${P[yopass_url]}"
            printf '   9) Yopass API           %s\n' "${P[yopass_api]}"
        fi
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1) ask v "Instance name, shown in broadcasts and help" "${P[name]}"; P[name]=$v; save_policy ;;
            2)
                blank
                note "open      anyone who messages the bot is subscribed at once."
                note "          They get the onion address and the shared secret."
                note "          Right for a public hangout, wrong for anything else."
                note "approval  requests wait for you to approve them."
                note "closed    nobody new gets in."
                blank
                ask v "Signups (open/approval/closed)" "${P[signups]}"
                case $v in open|approval|closed) P[signups]=$v; save_policy ;; *) oops "Not one of the three."; pause ;; esac ;;
            3) ask v "Tier a new subscriber lands on" "${P[default_tier]}"; P[default_tier]=$v; save_policy ;;
            4)
                blank
                note "A wall clock, not an idle timer: nobody tracks occupancy, so"
                note "a long call gets cut off when this expires."
                blank
                ask v "Room lifetime (e.g. 2h)" "${P[room_ttl]}"; P[room_ttl]=$v; save_policy ;;
            5) providers_menu ;;
            6) tiers_menu ;;
            7)
                blank
                note "inline  the onion and secret (or room URL) travel in the broadcast"
                note "        itself, in plain text on whatever chat network delivers it."
                note "link    each subscriber gets their own one-time Yopass link instead,"
                note "        minted against yopass_url/yopass_api (items 8/9 below;"
                note "        the public share.yopass.se by default). Opening it burns it."
                blank
                ask v "Credential delivery (inline/link)" "${P[creds_delivery]}"
                case $v in inline|link) P[creds_delivery]=$v; save_policy ;; *) oops "Not one of the two."; pause ;; esac ;;
            8)
                if [[ ${P[creds_delivery]} != link ]]; then
                    oops "Set credential delivery to link first (item 7)."; pause
                else
                    ask v "Yopass web URL (the share link's host)" "${P[yopass_url]}"
                    P[yopass_url]=$v; save_policy
                fi ;;
            9)
                if [[ ${P[creds_delivery]} != link ]]; then
                    oops "Set credential delivery to link first (item 7)."; pause
                else
                    ask v "Yopass API URL (what the yopass CLI mints against)" "${P[yopass_api]}"
                    P[yopass_api]=$v; save_policy
                fi ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Build and start
# --------------------------------------------------------------------------

# Prints the single reason the build is blocked, or nothing when it is ready.
build_blocker() {
    if ! _is_direct; then
        have docker || { echo "install docker (menu item 1)"; return; }
        docker compose version >/dev/null 2>&1 || { echo "install the docker compose v2 plugin"; return; }
        docker info >/dev/null 2>&1 || { echo "the docker daemon is not reachable"; return; }
    fi
    [[ -n "$(enabled_adapters)" ]] || { echo "configure an adapter"; return; }
    [[ -f $POLICY_FILE ]] || { echo "set a provider and policy"; return; }
    if ! _is_direct; then
        [[ -n ${ENVV[HOST_UID]:-} && -n ${ENVV[HOST_GID]:-} ]] || { echo "set the host identity"; return; }
    fi
    local pl dir name
    for pl in "${PARTY_LINES[@]}"; do
        uses_provider "$pl" || continue
        dir="${PARTYLINE_DIRS[$pl]}"
        name="${PARTYLINE_NAMES[$pl]}"
        if [[ ! -d $dir ]] && [[ ! -f "$ROOT/transports/$name/docker-compose.yml" ]]; then
            echo "no transport dir for $pl; set ${PARTYLINE_ENVVARS[$pl]} in .env or drop the provider"
            return
        fi
    done
    if uses_any_partyline && ! _is_direct; then
        [[ -n ${ENVV[DOCKER_GID]:-} ]] || { echo "no docker group id; the party line hooks need one"; return; }
    fi
    for a in $(enabled_adapters | tr ',' ' '); do
        case $a in
            telegram) [[ -n ${ENVV[TELEGRAM_TOKEN]:-} ]] || { echo "telegram has no token"; return; } ;;
            matrix)   [[ -n ${ENVV[MATRIX_PASSWORD]:-} ]] || { echo "matrix has no password"; return; } ;;
            xmpp)     [[ -n ${ENVV[XMPP_PASSWORD]:-} ]] || { echo "xmpp has no password"; return; } ;;
            mastodon) [[ -n ${ENVV[MASTODON_TOKEN]:-} ]] || { echo "mastodon has no access token"; return; } ;;
            email)    [[ -n ${ENVV[IMAP_PASSWORD]:-} ]] || { echo "email has no IMAP password"; return; } ;;
        esac
    done
}

build_and_start() {
    local blocker; blocker=$(build_blocker)
    if [[ -n $blocker ]]; then
        title "Build and start"
        blank
        oops "Blocked: $blocker"
        pause
        return
    fi

    title "Build and start"
    blank
    if ! image_built; then
        say "The first build compiles the whole dependency tree in release mode."
        say "${B}That takes 20+ minutes${NC} and ${B}7Gig of diskspace${NC} but you can walk away from it."
        say "Later builds reuse the layer cache and take a few minutes."
        blank
        note "If it looks stalled, docker stats will show a build container"
        note "pegging CPU, which means it is working."
        blank
    fi
    confirm "Start the build?" || return

    blank
    if ! dc build; then
        blank; oops "Build failed. The output above says why."; pause; return
    fi

    blank
    say "Checking the configuration before starting anything."
    blank
    if ! dc run --rm --no-deps partylinepagerd --check </dev/null; then
        blank; oops "The configuration has a problem. Nothing was started."; pause; return
    fi

    blank
    # Create these here rather than letting compose create them as root, which
    # would leave the daemon unable to write into its own bind mounts.
    mkdir -p "$STATE_DIR"
    adapter_on matrix && mkdir -p "$MATRIX_STORE_DIR"
    dc up -d --remove-orphans || { blank; oops "Start failed."; pause; return; }
    blank
    good "Running."
    note "A credential that is merely wrong (a bad token) shows up here, not in"
    note "the check above. Watch the log for a few seconds."
    blank
    read -rsn1 -p "  Press any key to follow the log. " _; blank
    follow_logs partylinepagerd
}

# --------------------------------------------------------------------------
# Service
# --------------------------------------------------------------------------

service_menu() {
    while true; do
        title "Service"
        blank
        if svc_running; then good "partylinepagerd is running"; else warn "partylinepagerd is stopped"; fi
        blank
        item 1 "Start"
        item 2 "Stop"
        item 3 "Restart"
        item 4 "Follow the log"
        item 5 "Check the configuration"
        item 6 "Container status"
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1) blank; dc up -d --remove-orphans; blank; pause ;;
            2) blank; dc stop; blank; pause ;;
            3) blank; dc restart; blank; pause ;;
            4) title "Log"; follow_logs partylinepagerd ;;
            5) title "Configuration check"; blank; dc run --rm --no-deps partylinepagerd --check </dev/null; blank; pause ;;
            6) title "Containers"; blank; dc ps; blank; pause ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Subscribers
# --------------------------------------------------------------------------

# Lists the roster and sets PICKED to the chosen endpoint, or empty.
PICKED=""
pick_endpoint() {
    local filter=${1:-} out line i=1
    PICKED=""
    if [[ -n $filter ]]; then out=$(ctl who --status "$filter" 2>&1); else out=$(ctl who 2>&1); fi

    if [[ -z $out || $out == nobody* ]]; then
        blank; say "Nobody${filter:+ with status $filter}."; blank; return 1
    fi

    local -a eps=()
    blank
    while IFS= read -r line; do
        [[ -z $line ]] && continue
        # `who` prints a header row first: show it, but never make it pickable.
        # Matching the literal header beats "skip line 1", which would silently
        # eat a real subscriber the day the header is removed again.
        if [[ $line == STATUS* ]]; then
            printf '       %s%s%s\n' "$DIM" "$line" "$NC"
            continue
        fi
        # who prints: status tier endpoint tz quiet rooms last-room
        local ep; ep=$(awk '{print $3}' <<< "$line")
        [[ -z $ep ]] && continue
        eps+=("$ep")
        printf '   %2d) %s\n' "$i" "$line"
        i=$((i + 1))
    done <<< "$out"
    blank
    (( ${#eps[@]} == 0 )) && { say "Nobody."; blank; return 1; }

    local c; ask c "Number, or blank to go back" ""
    [[ $c =~ ^[0-9]+$ ]] || return 1
    (( c < 1 || c > ${#eps[@]} )) && return 1
    PICKED="${eps[c-1]}"
    return 0
}

subscribers_menu() {
    while true; do
        title "Subscribers"
        blank
        item 1 "List everybody"
        item 2 "Approve" "someone waiting on the queue"
        item 3 "Change a tier"
        item 4 "Reset a quota" "so they may open a room again now"
        item 5 "Remove someone" "they may ask again"
        item 6 "Ban someone" "survives a resubscribe"
        item 7 "Unban"
        item 8 "Add an endpoint" "by hand, active immediately"
        item 9 "Set a display name" "so the roster and announcements show who this is"
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1) title "Roster"; blank; ctl who; blank; pause ;;
            2)
                title "Approve"
                if pick_endpoint pending; then
                    ctl approve "$PICKED"
                    note "They get a note on their own network the moment it takes effect."
                fi
                pause ;;
            3)
                title "Change tier"
                if pick_endpoint; then
                    say "Tiers: $(IFS=' '; echo "${TIER_NAME[*]}")"
                    ask v "New tier for $PICKED" "${P[default_tier]}"
                    [[ -n $v ]] && ctl tier "$PICKED" "$v"
                fi
                pause ;;
            4) title "Reset quota"; pick_endpoint && ctl reset-quota "$PICKED"; pause ;;
            5)
                title "Remove"
                if pick_endpoint; then
                    confirm "Remove $PICKED from the roster?" && ctl deny "$PICKED"
                fi
                pause ;;
            6)
                title "Ban"
                blank
                note "Banning somebody who never subscribed is fine: it blocks them"
                note "before they ever ask. Type the endpoint to ban a stranger."
                if pick_endpoint; then
                    confirm "Ban $PICKED for good?" && ctl ban "$PICKED"
                else
                    ask v "Endpoint to ban (transport:address), or blank" ""
                    [[ -n $v ]] && confirm "Ban $v for good?" && ctl ban "$v"
                fi
                pause ;;
            7)
                title "Unban"
                if pick_endpoint banned; then
                    ctl unban "$PICKED"
                    note "They are no longer banned, and not subscribed either."
                fi
                pause ;;
            8)
                title "Add an endpoint"
                blank
                note "transport:address, active immediately. Use apprise:<url> to"
                note "reach any of the services without a native adapter, e.g."
                note "apprise:ntfy://ntfy.sh/partylinepager"
                blank
                ask v "Endpoint" ""
                if [[ -n $v ]]; then
                    ask t "Tier" "${P[default_tier]}"
                    ctl add "$v" --tier "$t"
                fi
                pause ;;
            9)
                title "Set a display name"
                if pick_endpoint; then
                    ask v "Display name for $PICKED (blank to clear)" ""
                    ctl name "$PICKED" "$v"
                fi
                pause ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Rooms
#
# "Signal" in this script always means the messenger app. A room is opened, and
# what is waiting for an admin is a held request.
# --------------------------------------------------------------------------

# Sets LIVE_STATUS and LIVE_HOST, the endpoint that brought the current room
# up. `partylinepagerctl status` prints a `host:` line for this. Both are globals
# rather than a return value: command substitution runs in a subshell, so an
# assignment made in there would never reach the caller.
LIVE_STATUS=""
LIVE_HOST=""
read_live_room() {
    LIVE_STATUS=$(ctl status 2>/dev/null)
    LIVE_HOST=$(awk '/^host:/ {print $2}' <<< "$LIVE_STATUS")
}

rooms_menu() {
    while true; do
        title "Rooms"
        blank
        item 1 "What is live" "right now"
        item 2 "Clear the board" "close it and let the host redo it"
        item 3 "Close the room" "the host's quota stays spent"
        blank
        item 4 "Held requests" "requests waiting on you"
        item 5 "Release a request"
        item 6 "Discard a request"
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1) title "Live now"; blank; ctl status; blank; pause ;;
            2)
                title "Clear the board"
                blank
                read_live_room
                local out=$LIVE_STATUS
                sed 's/^/  /' <<< "$out"
                blank
                # `host:` is printed only for a room that parsed and is live,
                # which is a better test than looking for the word "room": the
                # unreadable-room.json status line contains that too.
                if [[ $out == *"unreadable room.json"* ]]; then
                    oops "room.json cannot be parsed, so there is nothing safe to close."
                    note "The daemon clears it at startup: restart the service from"
                    note "the Service menu, then stop any leftover room yourself."
                    pause; continue
                fi
                if [[ -z $LIVE_HOST ]]; then
                    say "Nothing is live, so there is nothing to clear."
                    note "If they are waiting on approval instead, discard the held"
                    note "request (item 6). A held request never spent any quota."
                    pause; continue
                fi
                say "This does two things, which is what somebody who opened the"
                say "wrong kind of room needs:"
                blank
                say "  1. closes the room, so the next one is not refused"
                say "  2. clears $LIVE_HOST's quota, so they may open one again now"
                blank
                note "Closing on its own does not refund the quota. They spent it"
                note "the moment the room came up, so without step 2 they wait out"
                note "their whole tier window before they can redo it."
                blank
                confirm "Clear the board?" || { pause; continue; }
                blank
                ctl close
                # Denied or banned since raising: say so rather than letting it
                # look like the whole action failed.
                ctl reset-quota "$LIVE_HOST" \
                    || warn "Could not reset $LIVE_HOST's quota; they may be off the roster."
                blank
                good "Done. The daemon tears the room down within five seconds."
                note "Tell them to open it again after that."
                pause ;;
            3)
                title "Close the room"
                blank
                ctl status
                blank
                note "The host's quota stays spent. Use item 2 instead if they"
                note "are going to open a replacement."
                blank
                confirm "Tear it down now?" && ctl close
                pause ;;
            4) title "Held requests"; blank; ctl pending; blank; pause ;;
            5|6)
                local verb="approve-request" word="Release"
                [[ $c == 6 ]] && { verb="deny-request"; word="Discard"; }
                title "$word a held request"
                local out; out=$(ctl pending 2>&1)
                if [[ $out == nothing* || -z $out ]]; then
                    blank; say "Nothing is waiting."; blank; pause; continue
                fi
                blank
                local -a ids=() line; local i=1
                while IFS= read -r line; do
                    [[ -z $line ]] && continue
                    # Same as pick_endpoint: show the header, never pick it.
                    if [[ $line == ID* ]]; then
                        printf '       %s%s%s\n' "$DIM" "$line" "$NC"
                        continue
                    fi
                    ids+=("$(awk '{print $1}' <<< "$line")")
                    printf '   %2d) %s\n' "$i" "$line"
                    i=$((i + 1))
                done <<< "$out"
                blank
                local n; ask n "Number, or blank to go back" ""
                if [[ $n =~ ^[0-9]+$ ]] && (( n >= 1 && n <= ${#ids[@]} )); then
                    if [[ $verb == deny-request ]]; then
                        note "Nothing was spent on a held request, so they can ask again"
                        note "straight away."
                        confirm "Discard ${ids[n-1]}?" && ctl "$verb" "${ids[n-1]}"
                    else
                        ctl "$verb" "${ids[n-1]}"
                    fi
                fi
                pause ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Instance
# --------------------------------------------------------------------------

instance_menu() {
    while true; do
        title "Instance"
        blank
        ctl status 2>/dev/null | sed 's/^/  /'
        blank
        item 1 "Pause" "refuse new rooms instance-wide"
        item 2 "Resume"
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1) blank; ctl pause; pause ;;
            2) blank; ctl resume; pause ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Signal device link
# --------------------------------------------------------------------------

# GET a path on signal-cli-rest-api from inside the compose network, since the
# service is only `expose`d and has no host port. Body goes to $2.
signal_api() {
    local path=$1 out=$2 cid
    if _is_direct; then
        # V1: signal-cli-rest-api runs locally, hit localhost directly.
        if command -v curl >/dev/null 2>&1; then
            curl -sf "http://127.0.0.1:8080$path" > "$out" 2>/dev/null && [[ -s $out ]] && return 0
        fi
        if command -v wget >/dev/null 2>&1; then
            wget -qO - "http://127.0.0.1:8080$path" > "$out" 2>/dev/null && [[ -s $out ]] && return 0
        fi
        return 1
    fi
    if dc exec -T signal-cli-rest-api curl -sf "http://127.0.0.1:8080$path" > "$out" 2>/dev/null \
       && [[ -s $out ]]; then return 0; fi
    if dc exec -T signal-cli-rest-api wget -qO - "http://127.0.0.1:8080$path" > "$out" 2>/dev/null \
       && [[ -s $out ]]; then return 0; fi
    cid=$(dc ps -q signal-cli-rest-api 2>/dev/null)
    [[ -z $cid ]] && return 1
    docker run --rm --network "container:$cid" curlimages/curl:latest \
        -sf "http://127.0.0.1:8080$path" > "$out" 2>/dev/null && [[ -s $out ]]
}

show_qr() {
    local png=$1
    if have chafa; then
        chafa -s 45x45 "$png"
    elif have viu; then
        viu -w 45 "$png"
    elif have timg; then
        timg -g 45x45 "$png"
    else
        return 1
    fi
}

signal_link_menu() {
    title "Signal device link"
    blank

    if ! adapter_on signal; then
        warn "The Signal adapter is off. Turn it on in Adapters first."
        pause; return
    fi

    say "Starting the signal-cli container."
    blank
    dc up -d signal-cli-rest-api >/dev/null 2>&1
    sleep 3

    local name png="$STATE_DIR/signal-link.png"
    ask name "Name for this device, as it appears in your linked devices list" "partylinepager"
    mkdir -p "$STATE_DIR"

    blank
    say "Fetching the link code."
    if ! signal_api "/v1/qrcodelink?device_name=$name" "$png"; then
        blank
        oops "Could not reach the signal-cli container."
        note "Check it is up: docker compose ps signal-cli-rest-api"
        pause; return
    fi

    title "Signal device link"
    blank
    if show_qr "$png"; then
        blank
    else
        warn "No terminal image viewer found, so the QR is saved as a file:"
        say "  $png"
        note "Install chafa to see it right here next time."
        blank
    fi
    say "On your phone: ${B}Signal > Settings > Linked Devices > +${NC}"
    blank

    say "Waiting for the link. Ctrl-C to stop waiting."
    local accounts="$STATE_DIR/.signal-accounts" i
    trap ':' INT
    for (( i = 0; i < 40; i++ )); do
        sleep 3
        if signal_api "/v1/accounts" "$accounts" 2>/dev/null; then
            if grep -q '[0-9]' "$accounts" 2>/dev/null; then
                trap - INT
                rm -f "$accounts"
                blank; good "Linked."
                note "Set the same number in Adapters > Signal if it is not already."
                pause; return
            fi
        fi
        printf '.'
    done
    trap - INT
    rm -f "$accounts"
    blank; blank
    warn "Gave up waiting. If you scanned it, check: docker compose logs signal-cli-rest-api"
    pause
}

# --------------------------------------------------------------------------
# Maintenance
# --------------------------------------------------------------------------

maintenance_menu() {
    while true; do
        title "Maintenance"
        blank
        item 1 "Back up" "this instance, to $BACKUP_ROOT"
        item 2 "Update" "pull, rebuild, restart"
        item 3 "Remove containers" "keeping all state"
        item 4 "Erase state" "destructive, asks to confirm"
        item 5 "Check dependencies" "cargo-deny, read-only"
        item 6 "Update dependencies" "cargo cooldown update, rewrites Cargo.lock"
        item 7 "Triage advisories" "diagnose failures, add ignores"
        blank
        item b "Back"
        blank
        local c; ask c "Choose"
        case $c in
            1)
                title "Backup"
                blank
                note "Everything irreplaceable is in this directory: the roster, the"
                note "quotas and the Signal device identity. Build caches are skipped."
                blank
                local dest="$BACKUP_ROOT/partylinepager-$(date +%Y%m%d-%H%M%S).tar.gz"
                mkdir -p "$BACKUP_ROOT"
                if tar czf "$dest" -C "$(dirname "$ROOT")" \
                        --exclude="$(basename "$ROOT")/.cache" \
                        --exclude="$(basename "$ROOT")/target" \
                        --exclude="$(basename "$ROOT")/.git" \
                        "$(basename "$ROOT")" 2>/dev/null; then
                    good "Written: $dest"
                    note "Unpack it on another host and docker compose up -d."
                else
                    oops "Backup failed."
                fi
                pause ;;
            2)
                title "Update"
                blank
                if [[ -d $ROOT/.git ]]; then
                    ( cd "$ROOT" && git pull ) || { oops "git pull failed."; pause; continue; }
                else
                    note "Not a git checkout, so nothing to pull. Rebuilding what is here."
                fi
                blank
                dc build && dc up -d --remove-orphans && good "Updated and running." || oops "Update failed."
                pause ;;
            3)
                title "Stop and remove"
                blank
                note "Containers go away. ./config/state, ./config/signal-cli and your config stay."
                blank
                confirm "Do it?" && { blank; dc down; }
                pause ;;
            4)
                title "Erase state"
                blank
                oops "This deletes ./config/state and ./config/signal-cli."
                say "That is the roster, the quotas, the live room and the Signal"
                say "device registration. The number has to be linked again."
                say "Your config files and this project stay."
                blank
                note "Back up first (menu item 1) if you are not certain."
                blank
                local typed
                ask typed "Type the instance name (${P[name]}) to confirm, or blank to cancel" ""
                if [[ $typed == "${P[name]}" ]]; then
                    dc down
                    rm -rf "$STATE_DIR" "$CONFIG_DIR/signal-cli"
                    good "Erased."
                else
                    say "Cancelled."
                fi
                pause ;;
            5)
                title "Check dependencies"
                blank
                if _is_direct; then
                    oops "Not available in V1 (full image) mode."
                    note "The audit scripts need docker compose and the test"
                    note "image, which are only in the V2 development checkout."
                    pause; continue
                fi
                note "Runs cargo-deny against Cargo.lock: banned packages,"
                note "RustSec advisories, source pinning. Read-only: it does"
                note "not rewrite Cargo.lock and does not compile anything."
                note "Publish age is enforced at update time, by item 6."
                blank
                "$ROOT/hooks/audit-cargo-deps.sh" && good "Clean." || oops "Findings above."
                pause ;;
            6)
                title "Update dependencies"
                blank
                if _is_direct; then
                    oops "Not available in V1 (full image) mode."
                    note "The update scripts need docker compose and the test"
                    note "image, which are only in the V2 development checkout."
                    pause; continue
                fi
                oops "This rewrites Cargo.lock."
                note "cargo-cooldown holds back anything published inside the"
                note "cooldown window, then the same check as item 5 runs"
                note "against the result. If anything fails, the previous"
                note "Cargo.lock is put back, so you end up with a lockfile"
                note "that passed or the one you started with."
                blank
                confirm "Do it?" && {
                    blank
                    "$ROOT/hooks/update-cargo-deps.sh" && good "Cargo.lock updated." || oops "Update failed; see output above."
                }
                pause ;;
            7)
                title "Triage advisories"
                blank
                if _is_direct; then
                    oops "Not available in V1 (full image) mode."
                    note "The triage script needs docker compose and the test"
                    note "image, which are only in the V2 development checkout."
                    pause; continue
                fi
                note "Runs the audit, then for each advisory finding:"
                note "  - Checks whether a compatible update exists"
                note "  - If yes: directs you to Update dependencies (item 6)"
                note "  - If blocked: offers to add an ignore to deny.toml"
                blank
                "$ROOT/hooks/triage-advisory.sh" && good "All findings resolved." || oops "Some findings remain; see output above."
                pause ;;
            b|B|"") return ;;
        esac
    done
}

# --------------------------------------------------------------------------
# Home
# --------------------------------------------------------------------------

setup_home() {
    local blocker; blocker=$(build_blocker)
    local adapters; adapters=$(enabled_adapters)

    title "PartylinePager"
    blank
    say "Not running yet. Work down this list."
    blank

    local sys
    if _is_direct; then
        sys="${GRN}ok (V1)${NC}"
    elif have docker && docker compose version >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        sys="${GRN}ok${NC}"
    else
        sys="${RED}needs attention${NC}"
    fi

    item 1 "System check" "$sys"
    item 2 "Host identity" "$(identity_state)"
    if [[ -n $adapters ]]; then
        item 3 "Adapters" "$adapters"
    else
        item 3 "Adapters" "${RED}none${NC}   required"
    fi
    item 4 "Policy" "${P[providers]}, signups ${P[signups]}"
    if adapter_on signal; then
        item 5 "Signal device link" \
            "$(image_built && echo "ready to link" || echo "after the build")"
    else
        item 5 "Signal device link" "${DIM}not needed${NC}"
    fi
    blank
    if [[ -n $blocker ]]; then
        item 9 "BUILD & START" "${YEL}blocked: $blocker${NC}"
    else
        item 9 "BUILD & START" "${GRN}ready${NC}"
    fi
    blank
    item q "Quit"
    blank
    note "Items 1-5 take about five minutes. The first build then takes 20+"
    note "minutes and you can walk away from it. README.md has the detail."
    blank
}

admin_home() {
    title "PartylinePager"
    blank
    local state
    if svc_running; then state="${GRN}running${NC}"; else state="${YEL}stopped${NC}"; fi
    printf '  %b · %s · %s%s\n' \
        "$state" "$(enabled_adapters)" "$(count "$(sub_count)" subscriber)" \
        "$(is_paused && printf ' · %spaused%s' "$YEL" "$NC")"
    printf '  %s\n' "$RULE"
    blank
    # 3, 4 and 5 are deliberately the same destinations they are on the setup
    # screen. Those three are the only ones both screens have, and an admin who
    # learned "4 is policy" while setting the instance up should not have to
    # learn a second number for it the moment the build finishes.
    item 1 "Service" "start, stop, restart, logs"
    item 2 "Subscribers" "approve, tier, quota, ban"
    item 3 "Adapters" "add, remove, re-enter credentials"
    item 4 "Policy" "signups, tiers, room lifetime, providers"
    item 5 "Signal link" "link a phone"
    blank
    item 6 "Rooms" "what is live, held requests"
    item 7 "Instance" "pause, resume, status"
    item 8 "Maintenance" "back up, update, remove"
    blank
    item q "Quit"
    blank
}

main() {
    mkdir -p "$CONFIG_DIR"
    load_env
    _resolve_pl_dirs
    load_adapters
    load_policy

    while true; do
        if configured && image_built; then
            admin_home
            local c; ask c "Choose"
            case $c in
                1) service_menu ;;
                2) subscribers_menu ;;
                3) adapters_menu ;;
                4) policy_menu ;;
                5) signal_link_menu ;;
                6) rooms_menu ;;
                7) instance_menu ;;
                8) maintenance_menu ;;
                q|Q) clear; exit 0 ;;
            esac
        else
            setup_home
            local c; ask c "Choose"
            case $c in
                1) system_check_menu ;;
                2) identity_menu ;;
                3) adapters_menu ;;
                4) policy_menu ;;
                5) signal_link_menu ;;
                9) build_and_start ;;
                q|Q) clear; exit 0 ;;
            esac
        fi
    done
}

main "$@"
