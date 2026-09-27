[![PartyLinePager](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/posts/party-line-pager--tor-i2p-rns-call-pager.svg)](https://gitlab.com/MarcusHoltz/party-line-pager)

[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=for-the-badge)](https://gitlab.com/MarcusHoltz/party-line-pager/-/blob/main/LICENSE)
[![Source: GitLab](https://img.shields.io/badge/source-GitLab-orange?style=for-the-badge&logo=gitlab)](https://gitlab.com/MarcusHoltz/party-line-pager)
[![Source: GitHub](https://img.shields.io/badge/source-GitHub-black?style=for-the-badge&logo=github)](https://github.com/MarcusHoltz/party-line-pager)

# marcusholtz/party-line-pager

Open a room on a chat network, and everyone who subscribed on **any** chat network gets a way into it. Supports Tor, I2P, Reticulum party lines, and WebRTC browser links. Fans credentials to nine chat networks. No database, no accounts, nothing remembered.

**This is the daemon-only image**: one container per network/transport you enable, no ttyd, no bundled wizard, no port 7681. 

There is no separate "wizard edition" of this image. 

**The setup wizard** (`party-line-pager.sh`) is a script in the [git repo](https://github.com/MarcusHoltz/party-line-pager), you can download. 

It runs on your host, writes `.env`, `policy.toml` and `adapters.toml` for you, and then calls `docker compose` to pull and run this exact image.

## Supported Architectures

| Architecture | Tag |
| :---: | --- |
| x86-64 | `amd64` |

## Quick Start: Wizard (recommended)

```sh
git clone https://github.com/MarcusHoltz/party-line-pager.git && cd party-line-pager
./party-line-pager.sh
```
  - Menu walks you through: adapters → policy → which providers
  - Writes config/policy.toml, config/adapters.toml, .env
  - Sets COMPOSE_FILE to pull marcusholtz/party-line-pager:latest (not build)
  - Runs docker compose up -d for you
  - Same script doubles as admin console after setup (roster, pause/resume, Signal link)
  - To hand-edit the TOML yourself: quit the script first, it owns those files while running

## Quick Start: Manual (no wizard)

For scripted or config-management deployments where you'd rather not clone
the whole repo. Requires a `policy.toml` and `adapters.toml` already sitting
in `./config`, written against the [config reference](https://github.com/MarcusHoltz/party-line-pager/tree/main/docs/configuration).

### docker-compose

Save as `docker-compose.yml`, then run `docker compose up -d`:

```yaml
services:
  party-line-pagerd:
    image: marcusholtz/party-line-pager:latest
    container_name: party-line-pagerd
    restart: unless-stopped
    user: "1000:1000"
    volumes:
      - ./config:/etc/party-line-pager
    environment:
      - RUST_LOG=info
      - TELEGRAM_TOKEN=your-bot-token
    command: ["--state", "/etc/party-line-pager/state"]
```

### docker cli

```bash
docker run -d \
  --name party-line-pagerd \
  --restart unless-stopped \
  --user 1000:1000 \
  -v ./config:/etc/party-line-pager \
  -e RUST_LOG=info \
  -e TELEGRAM_TOKEN=your-bot-token \
  marcusholtz/party-line-pager:latest \
  --state /etc/party-line-pager/state
```

### First run

1. `policy.toml` and `adapters.toml` are already in `./config` (see above)
2. `--user` matches your host uid:gid, or `./config/state` ends up root-owned
3. Only the adapters you set a token for should be `[enabled = true]` in `adapters.toml`
4. `docker compose run --rm party-line-pagerd --check` validates config before starting anything
5. `docker compose up -d` starts the daemon

No web terminal, no port to publish. Administration is `docker compose exec` only, same as any headless deployment.

```bash
docker compose exec party-line-pagerd party-line-pagerctl who
docker compose exec party-line-pagerd party-line-pagerctl approve telegram:123456789
```

## Parameters

### Core

| Parameter | Function |
| :---: | --- |
| `-v /etc/party-line-pager` | `policy.toml` and `adapters.toml` (read by the daemon), plus `state/` (roster and room state, auto-created) |
| `--state /etc/party-line-pager/state` | Command argument, not an env var: where state is written inside the mount |
| `-e RUST_LOG=info` | Log verbosity |
| `--user 1000:1000` | Match your host uid:gid so `./config/state` isn't written as root |

### Adapters

Enable an adapter by setting `enabled = true` under its section in `adapters.toml`, then pass the matching secret through `-e`. Only adapters you pass a secret for receive room notifications.

| Parameter | Function |
| :---: | --- |
| `-e TELEGRAM_TOKEN` | Bot token from @BotFather (`[telegram]`) |
| `-e MATRIX_PASSWORD` | Matrix bot account password (`[matrix]`) |
| `-e IRC_PASSWORD` | NickServ password for SASL PLAIN (`[irc]`); omit to connect without logging in |
| `-e XMPP_PASSWORD` | XMPP account password (`[xmpp]`) |
| `-e MASTODON_TOKEN` | Mastodon application token (`[mastodon]`) |
| `-e IMAP_PASSWORD` | IMAP account password (`[email]`) |
| `-e SMTP_PASSWORD` | SMTP account password (`[email]`) |
| `-e MATTERMOST_TOKEN` | Personal or bot access token (`[mattermost]`) |
| `-e DISCORD_TOKEN` | Bot token from the Discord Developer Portal (`[discord]`) |

Signal (`[signal]`) takes no token here: it needs a second container, [`bbernhard/signal-cli-rest-api`](https://hub.docker.com/r/bbernhard/signal-cli-rest-api), linked or registered separately and mounted at `/home/.local/share/signal-cli`. Apprise (`apprise:` endpoints, added at runtime, not in `adapters.toml`) needs [`caronc/apprise`](https://hub.docker.com/r/caronc/apprise) as a stateless sidecar.

### Party lines (Tor, I2P, Reticulum)

Off by default. Turning one on in `policy.toml` (`[provider.tor]`, `[provider.i2p]`, `[provider.rns]`) means this container also needs the Docker socket, because the provider hooks bring the transport up and down as its own container.

| Parameter | Function |
| :---: | --- |
| `-v /var/run/docker.sock` | Only if any `[provider.*]` party line is enabled |
| `-e DOCKER_GID` | The docker group's gid on the host (`getent group docker \| cut -d: -f3`), passed to `group_add` |
| `-v <transport-dir>` | Bind-mounted at the identical path inside and outside the container; holds the transport's own compose file, pulls a published relay image, no checkout needed |

`[provider.web]` needs neither. If you're only running `web` rooms, leave the socket out entirely: `docker compose config | grep -c docker.sock` should print `0`.

## How It Works

A subscriber sends a command (`partyline` or `web`) on any connected chat network. The engine:

1. Spins up a fresh room on the requested transport (new .onion, .b32.i2p, destination hash, or URL)
2. Fans the connection details to every subscriber whose quiet hours are closed
3. Tears the room down when the TTL expires

One room at a time. No occupancy tracking, no catch-up, no record of who joined.

## Verifying a Deployment

```bash
# Config check before ever starting the daemon
docker compose run --rm party-line-pagerd --check

# Logs: look for "ok" from the check, then normal startup
docker compose logs party-line-pagerd

# Roster check
docker compose exec party-line-pagerd party-line-pagerctl who
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
