[![PartyLinePager](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/posts/party-line-pager--tor-i2p-rns-call-pager.svg)](https://gitlab.com/MarcusHoltz/party-line-pager)

[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=for-the-badge)](https://gitlab.com/MarcusHoltz/party-line-pager/-/blob/main/LICENSE)
[![Source: GitLab](https://img.shields.io/badge/source-GitLab-orange?style=for-the-badge&logo=gitlab)](https://gitlab.com/MarcusHoltz/party-line-pager)
[![Source: GitHub](https://img.shields.io/badge/source-GitHub-black?style=for-the-badge&logo=github)](https://github.com/MarcusHoltz/party-line-pager)

# marcusholtz/party-line-pager-full

Open a room on a chat network, and everyone who subscribed on **any** chat network gets a way into it. Supports Tor, I2P, Reticulum party lines, and WebRTC browser links. Fans credentials to nine chat networks. No database, no accounts, nothing remembered.

## Supported Architectures

| Architecture | Tag |
| :---: | --- |
| x86-64 | `amd64` |

## Quick Start: Wizard Mode

Pull and run the web terminal. Configure adapters, policy, and providers through a browser.

### docker-compose (recommended)

Save as `docker-compose.yml`, then run `docker compose up -d`:

```yaml
services:
  party-line-pager:
    image: marcusholtz/party-line-pager-full:latest
    container_name: party-line-pager
    restart: unless-stopped
    ports:
      - "7681:7681"
    volumes:
      - ./config:/config
    environment:
      - TTYD_CREDENTIAL=admin:changeme
```

Open `http://<host>:7681` and follow the wizard.

### docker cli

```bash
docker run -d \
  --name party-line-pager \
  --restart unless-stopped \
  -p 7681:7681 \
  -v ./config:/config \
  -e TTYD_CREDENTIAL=admin:changeme \
  marcusholtz/party-line-pager-full:latest
```

### First run

1. Open `http://<host>:7681` in a browser
2. The wizard walks you through adapter setup (Telegram, Matrix, IRC, etc.)
3. Configure which providers to enable (`tor`, `i2p`, `rns`, `web`)
4. Config files are written to `/config` automatically
5. Start the daemon from the wizard menu

About five minutes of configuration, then a 20+ minute first build you can walk away from.

## Quick Start: Headless Daemon

Run the daemon without a web terminal. Requires valid config from a prior wizard run.

### docker-compose

```yaml
services:
  party-line-pager:
    image: marcusholtz/party-line-pager-full:latest
    container_name: party-line-pager
    restart: unless-stopped
    volumes:
      - ./config:/config
    environment:
      - PLP_HEADLESS=1
      - PLP_PROVIDERS=tor,i2p,rns,web
      - TELEGRAM_TOKEN=your-bot-token
```

### docker cli

```bash
docker run -d \
  --name party-line-pager \
  --restart unless-stopped \
  -v ./config:/config \
  -e PLP_HEADLESS=1 \
  -e PLP_PROVIDERS=tor,i2p,rns,web \
  -e TELEGRAM_TOKEN=your-bot-token \
  marcusholtz/party-line-pager-full:latest
```

No ttyd, no wizard, no port 7681. Administration is `docker exec` only:

```bash
docker exec party-line-pager party-line-pagerctl --state /config/state roster
docker exec party-line-pager party-line-pagerctl --state /config/state pause
```

## Parameters

### Core

| Parameter | Function |
| :---: | --- |
| `-p 7681:7681` | Web terminal (wizard and admin) |
| `-v /config` | Persistent config, state, and secrets |
| `-e TTYD_CREDENTIAL` | Basic auth for the web terminal (`user:password`) |
| `-e PLP_PROVIDERS` | Comma-separated transports: `tor`, `i2p`, `rns`, `web` |
| `-e PLP_AUTO_START=0` | Start daemon on boot (`0`/`1`; requires valid config) |
| `-e PLP_HEADLESS=0` | Daemon-only, no web terminal (`0`/`1`) |
| `-e PLP_WEB_BASE_URL` | Video call instance URL for `web` rooms |

### Adapters

Enable an adapter by providing its credentials. Only enabled adapters receive room notifications.

| Parameter | Function |
| :---: | --- |
| `-e TELEGRAM_TOKEN` | Bot token from @BotFather |
| `-e DISCORD_TOKEN` | Bot token from the Discord Developer Portal |
| `-e MATRIX_HOMESERVER` | Matrix homeserver URL |
| `-e MATRIX_USER` | Matrix bot username |
| `-e MATRIX_PASSWORD` | Matrix bot password |
| `-e IRC_SERVER` | IRC server hostname |
| `-e IRC_NICK=party-line-pager` | IRC bot nickname |
| `-e IRC_CHANNEL` | IRC channel to idle in |
| `-e IRC_PASSWORD` | NickServ password for SASL PLAIN |
| `-e XMPP_JID` | XMPP JID for the bot |
| `-e XMPP_PASSWORD` | XMPP account password |
| `-e MASTODON_INSTANCE` | Mastodon instance URL |
| `-e MASTODON_TOKEN` | Mastodon application token |
| `-e MATTERMOST_URL` | Mattermost server URL |
| `-e MATTERMOST_TOKEN` | Mattermost access token |
| `-e SIGNAL_NUMBER` | Signal phone number in E.164 format |
| `-e EMAIL_SMTP_HOST` | SMTP server hostname |
| `-e EMAIL_FROM` | From address for outgoing email |

## How It Works

A subscriber sends a command (`partyline` or `web`) on any connected chat network. The engine:

1. Spins up a fresh room on the requested transport (new .onion, .b32.i2p, destination hash, or URL)
2. Fans the connection details to every subscriber whose quiet hours are closed
3. Tears the room down when the TTL expires

One room at a time. No occupancy tracking, no catch-up, no record of who joined.

## Verifying a Deployment

```bash
# Logs: look for "party-line-pagerd is up"
docker logs party-line-pager

# Processes: tini, ttyd (if not headless), party-line-pagerd
docker exec party-line-pager ps aux

# Roster check:
docker exec party-line-pager party-line-pagerctl --state /config/state roster
```

## The Party Line Trifecta

Three transports, same encryption, same TUI:

| | | |
|---|---|---|
| [![Tor Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--tor-onion-router-overlay-network.jpg)](https://gitlab.com/MarcusHoltz/tor-party-line) | [![I2P Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--invisible-internet-project-i2p-garlic-roter.jpg)](https://gitlab.com/MarcusHoltz/i2p-party-line) | [![Reticulum Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--reticulum-network-stack.jpg)](https://gitlab.com/MarcusHoltz/reticulum-party-line) |
| [Tor Party Line](https://gitlab.com/MarcusHoltz/tor-party-line) | [I2P Party Line](https://gitlab.com/MarcusHoltz/i2p-party-line) | [Reticulum Party Line](https://gitlab.com/MarcusHoltz/reticulum-party-line) |

PartyLinePager orchestrates all three (plus WebRTC) and fans credentials across nine chat networks.

## License

MIT
