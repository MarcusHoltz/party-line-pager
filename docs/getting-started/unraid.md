# Full Image (Unraid / One Container)

Everything in one container. No Docker socket required.

The full image (`deploy/Dockerfile.full`) bundles the daemon, all three
transport runtimes (Tor, I2P, Reticulum), Signal in native mode, and a 
browser-based admin terminal via ttyd. 

!!! warning inline "Image size is ~1.1GB"
The full, single image, version uses about **1.1 GB** of space.

<br/>


* * *

## Build

```sh
docker build -f deploy/Dockerfile.full -t partylinepager-full .
```

## Quick start

```sh
docker run -d --name partylinepager \
  -p 7681:7681 \
  -v ./config:/config \
  -e TTYD_CREDENTIAL=admin:changeme \
  partylinepager-full
```

Open `http://<your-server>:7681` in a browser. The wizard walks
you through adapters, policy, and Signal linking. Config files
are written to the `/config` volume.

!!! warning "Set TTYD_CREDENTIAL"

    Without it, anyone who can reach port 7681 has full admin
    access. The entrypoint prints a loud warning but still
    starts.

## Three operating modes

### 1. Wizard mode (first-time setup)

```sh
docker run -d --name partylinepager \
  -p 7681:7681 \
  -v ./config:/config \
  -e TTYD_CREDENTIAL=admin:changeme \
  partylinepager-full
```

Browser terminal at `http://<host>:7681`. The wizard writes
config files. No daemon runs until you tell it to start.

### 2. Wizard + daemon (admin terminal with daemon running)

```sh
docker run -d --name partylinepager \
  -p 7681:7681 \
  -v ./config:/config \
  -e PLP_AUTO_START=1 \
  -e TTYD_CREDENTIAL=admin:changeme \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e PLP_PROVIDERS="tor,i2p,rns,web" \
  partylinepager-full
```

The daemon starts in the background on boot, then ttyd opens
the wizard. Browse to `http://<host>:7681` to manage the
running instance.

### 3. Headless (daemon only, no browser terminal)

```sh
docker run -d --name partylinepager \
  -v ./config:/config \
  -e PLP_HEADLESS=1 \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e PLP_PROVIDERS="tor,i2p,rns,web" \
  partylinepager-full
```

No ttyd, no wizard, no port 7681. The daemon runs as PID 1.
Administration is `docker exec` only:

```sh
docker exec partylinepager partylinepagerctl \
  --state /config/state roster
docker exec partylinepager partylinepagerctl \
  --state /config/state pause
```

## Recipes

**Telegram only, web rooms, browser admin:**

```sh
docker run -d --name partylinepager \
  -p 7681:7681 \
  -v ./config:/config \
  -e PLP_AUTO_START=1 \
  -e TTYD_CREDENTIAL=admin:changeme \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e PLP_PROVIDERS="web" \
  partylinepager-full
```

**Telegram + Signal + IRC, all party lines, headless:**

```sh
docker run -d --name partylinepager \
  -v ./config:/config \
  -e PLP_HEADLESS=1 \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e SIGNAL_NUMBER="+15555550100" \
  -e IRC_SERVER="irc.libera.chat" \
  -e IRC_NICK="partylinepager" \
  -e PLP_PROVIDERS="tor,i2p,rns,web" \
  partylinepager-full
```

**Quick foreground test** (removed on exit, no volume):

```sh
docker run --rm --name plp-test \
  -e PLP_HEADLESS=1 \
  -e TELEGRAM_TOKEN="your-bot-token" \
  partylinepager-full
```

## Verifying a deployment

```sh
# Logs: look for "partylinepagerd is up" and
# "Listening on port: 7681"
docker logs partylinepager

# Processes: tini, ttyd (if not headless), partylinepagerd
docker exec partylinepager ps aux

# Web UI (wizard/auto-start modes only):
curl -u admin:changeme http://localhost:7681/

# Daemon responding:
docker exec partylinepager partylinepagerctl \
  --state /config/state roster
```

## Persistent config

Bind-mount a host directory to `/config`:

```
-v /path/to/config:/config
```

The entrypoint copies `config/policy.example.toml` and
`config/adapters.example.toml` on first run if no `policy.toml`
exists yet. After that, the files in the volume are yours.

Env vars (`TELEGRAM_TOKEN`, etc.) regenerate `adapters.toml`
on every start, overriding whatever the wizard wrote. To use
wizard-written config without env var override, drop the `-e`
adapter flags and just mount the volume.

## Environment variables

### Core settings

| Variable | Default | Effect |
|---|---|---|
| `PLP_HEADLESS` | `0` | `1` runs the daemon as PID 1, no ttyd |
| `PLP_AUTO_START` | `0` | `1` starts daemon in background, then ttyd |
| `PLP_PROVIDERS` | (none) | Comma-separated: `tor,i2p,rns,web` |
| `CONFIG_DIR` | `/config` | Where policy.toml/adapters.toml live |
| `STATE_DIR` | `$CONFIG_DIR/state` | Where roster and room state live |
| `TTYD_PORT` | `7681` | Browser terminal port |
| `TTYD_CREDENTIAL` | (none) | `user:password` for ttyd basic auth |

### Adapter credentials

Setting any of these regenerates `adapters.toml` on startup.
Env vars take precedence over wizard-written config.

| Variable | Enables |
|---|---|
| `TELEGRAM_TOKEN` | `[telegram]` adapter |
| `DISCORD_TOKEN` | `[discord]` adapter |
| `SIGNAL_NUMBER` | `[signal]` adapter + starts signal-cli-rest-api |

### Matrix

| Variable | Default | Notes |
|---|---|---|
| `MATRIX_HOMESERVER` | (none) | Enables `[matrix]` adapter |
| `MATRIX_USER` | (none) | Bot's localpart (not full MXID) |
| `MATRIX_PASSWORD` | (none) | Bot account password |
| `MATRIX_STORE_PATH` | `/var/lib/partylinepager/matrix` | E2EE key store |

### IRC

| Variable | Default | Notes |
|---|---|---|
| `IRC_SERVER` | (none) | Enables `[irc]` adapter |
| `IRC_PORT` | `6697` | Server port |
| `IRC_NICK` | `partylinepager` | Bot's nickname |
| `IRC_TLS` | `true` | TLS for the connection |
| `IRC_CHANNEL` | (none) | Channel to idle in (for discoverability) |
| `IRC_ACCOUNT` | (none) | Services account, if different from nick |
| `IRC_PASSWORD` | (none) | NickServ/SASL password |

### XMPP

| Variable | Default | Notes |
|---|---|---|
| `XMPP_JID` | (none) | Enables `[xmpp]` adapter |
| `XMPP_PASSWORD` | (none) | Account password |
| `XMPP_TLS` | `true` | STARTTLS negotiation |

### Mastodon

| Variable | Default | Notes |
|---|---|---|
| `MASTODON_INSTANCE` | (none) | Enables `[mastodon]` adapter. Base URL, e.g. `https://mastodon.social` |
| `MASTODON_TOKEN` | (none) | Application access token |
| `MASTODON_POLL_INTERVAL` | `30s` | How often to check for mentions |

### Mattermost

| Variable | Default | Notes |
|---|---|---|
| `MATTERMOST_URL` | (none) | Enables `[mattermost]` adapter. Base URL |
| `MATTERMOST_TOKEN` | (none) | Personal or bot access token |

### Email

| Variable | Default | Notes |
|---|---|---|
| `EMAIL_SMTP_HOST` | (none) | Enables `[email]` adapter |
| `EMAIL_SMTP_PORT` | `587` | SMTP port (465=implicit TLS, 587/25=STARTTLS) |
| `EMAIL_SMTP_USER` | (none) | SMTP login |
| `EMAIL_SMTP_PASSWORD` | (none) | SMTP password |
| `EMAIL_IMAP_HOST` | same as SMTP | IMAP server |
| `EMAIL_IMAP_PORT` | `993` | IMAP port |
| `EMAIL_IMAP_USER` | same as SMTP user | IMAP login |
| `EMAIL_IMAP_PASSWORD` | same as SMTP password | IMAP password |
| `EMAIL_MAILBOX` | `INBOX` | Mailbox to watch |
| `EMAIL_FROM` | same as SMTP user | Envelope and header From |
| `EMAIL_TLS` | `true` | Encrypt both IMAP and SMTP |
| `EMAIL_POLL_INTERVAL` | `60s` | How often to check for mail |

### Signal

| Variable | Default | Notes |
|---|---|---|
| `SIGNAL_NUMBER` | (none) | Enables `[signal]` adapter, starts signal-cli-rest-api |
| `SIGNAL_REST_URL` | `http://127.0.0.1:8080` | signal-cli-rest-api URL (full image runs it locally) |

### Web provider

| Variable | Default | Notes |
|---|---|---|
| `PLP_WEB_BASE_URL` | `https://p2p.mirotalk.com` | WebRTC instance for room URLs |
| `PLP_WEB_PATH` | `join` | Path segment between base URL and room slug |

## What is in the image

| Component | Path | Purpose |
|---|---|---|
| partylinepagerd | `/usr/local/bin/` | The daemon |
| partylinepagerctl | `/usr/local/bin/` | CLI admin tool |
| yopass | `/usr/local/bin/` | Link-mode credential delivery |
| mkp224o | `/usr/local/bin/` | Vanity .onion address generator |
| ttyd | `/usr/local/bin/` | Browser terminal for the wizard |
| tor | `/usr/sbin/` | Tor transport |
| i2pd | `/usr/bin/` | I2P transport |
| signal-cli-native | `/usr/bin/` | Signal (GraalVM, no JVM) |
| signal-cli-rest-api | `/usr/local/bin/` | Signal REST wrapper (native mode) |
| python3 + RNS + LXMF | `/usr/bin/` | Reticulum transport |
| apprise | (pip) | Fanout to ntfy/Gotify/Pushover/etc. |
| socat, openssl, qrencode | system | Relay, encryption, QR codes |
| tor-party-line | `/opt/tor-party-line/` | Tor audio relay script |
| i2p-party-line | `/opt/i2p-party-line/` | I2P audio relay script |
| reticulum-party-line | `/opt/reticulum-party-line/` | Reticulum audio relay script |

## Full image transport notes

All three transport relays (Tor, I2P, Reticulum) default to
port 7777. In the standard deployment, each runs in its own
container with its own network namespace, so there is no
conflict. In the full image, they share the container's network
stack. Only one transport relay can listen at a time.

The daemon opens rooms one provider at a time (the subscriber
picks `tor`, `i2p`, `rns`, or `web`), so this is not a
practical limitation unless two rooms on different transports
overlap within the same `room_ttl` window.

## Docker run syntax reminder

All options (`-e`, `--name`, `-p`, `-v`) go **before** the
image name. Docker treats the first bare word after the options
as the image; anything after it becomes the container's command.

```
docker run [OPTIONS] IMAGE
              ^        ^
              |        +-- always last
              +-- -d, --name, -p, -v, -e all go here
```
