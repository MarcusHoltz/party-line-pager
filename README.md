# PartyLinePager

Open a room on one chat network, and everyone who subscribed on **any** chat
network gets a way into it.

There are four kinds of room, one command each. Three are the same party line over
a different network, and the fourth is a plain browser link:

- **`tor`** brings up a [tor-party-line](https://gitlab.com/MarcusHoltz/tor-party-line)
  relay behind a brand new onion address, with a freshly generated shared
  secret. Maximum privacy, needs Tor and a terminal at the other end, takes one
  to three minutes to come up.
- **`i2p`** does the same over I2P, behind a brand new `.b32.i2p` address.
  Same privacy story, same terminal client, and it comes up in about 23 seconds
  instead of one to three minutes. No port to open: a firewalled router is the
  normal, intended state.
- **`rns`** does the same over [Reticulum](https://reticulum.network/),
  behind a brand new 32-character destination hash. Fastest of the three, no
  port to open, and it reaches through NAT on both ends by way of public
  transport nodes. **It is not onion routing**: the relay sees every caller's
  IP address and the address is announced across the network. Reach for it when
  getting connected is the point, and for one of the other two when anonymity
  is.
- **`web`** mints a link to a MiroTalk-style WebRTC room, on your own MiroTalk,
  Jitsi, or any other self-hosted alternative. Instant, works in any browser,
  no secret to type: the unguessable URL *is* the access control (unless you
  configure a static room name instead, see `[provider.web]` in the
  [policy reference](docs/configuration/policy.md)).

Whichever it is, the partyline pager fans the details out to every subscriber
whose quiet hours are closed, and a timer closes the room. Nothing is
remembered: no occupancy tracking, no catch-up for the people who were asleep,
no record of who joined.

One room is live at a time, whichever command opened it. An admin configures only
the providers they want, and the other commands stop existing.

Nine chat networks have native adapters. Everything else Apprise can reach
(130+ services) works by adding an endpoint by hand.

```
  Telegram   -+                                +- tor ---------> tor-party-line
  Matrix     -|                                |                 (fresh .onion)
  Signal     -|                                +- i2p ---------> i2p-party-line
  IRC        -+-> adapters --> engine -> policy-+                 (fresh .b32.i2p)
  XMPP       -|                  |             +- rns ----------> reticulum-party-line
  Mastodon   -|                  |             |                 (fresh hash)
  Email      -|                  |             +- web ---------> room URL
  Mattermost -|                  |             |                 (no secret)
  Discord    -+                  |             +- fanout --> every subscriber,
                                 |                          minus quiet hours
                                 +- state/ (JSON files, no database)
                                         ^
                                 party-line-pagerctl over SSH
```

---

## The Trifecta

PartyLinePager orchestrates three sibling projects, each a standalone encrypted push-to-talk voice app built for a different transport:


| | | |
|---|---|---|
| [![Tor Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--tor-onion-router-overlay-network.jpg)](https://gitlab.com/MarcusHoltz/tor-party-line) | [![I2P Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--invisible-internet-project-i2p-garlic-roter.jpg)](https://gitlab.com/MarcusHoltz/i2p-party-line) | [![Reticulum Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--reticulum-network-stack.jpg)](https://gitlab.com/MarcusHoltz/reticulum-party-line) |
| [Tor Party Line](https://gitlab.com/MarcusHoltz/tor-party-line) | [I2P Party Line](https://gitlab.com/MarcusHoltz/i2p-party-line) | [Reticulum Party Line](https://gitlab.com/MarcusHoltz/reticulum-party-line) |



Each ships in triplicate: same TUI, same encryption, same PTT semantics, different wire. PartyLinePager opens a room on any of them (or a WebRTC link), fans the credentials to your roster across nine chat networks, and tears it down when the timer runs out.

---

## Status

Working today: all nine adapters, all four room providers (`tor`, `i2p`,
`rns` and `web`), the policy engine, rolling quotas, quiet hours,
hold-for-approval, the provider hook contract, TTL teardown, crash recovery,
and the admin CLI.

**Six of the nine adapters that can be, are verified against live servers**
rather than mocks, each in a throwaway container this repository defines and
starts: IRC, Matrix, XMPP, Mastodon, email and Mattermost. Discord is manually
verified against a real bot and server. Telegram and Signal have no live suite:
neither can be self-hosted.

267 tests, none reaching the public internet.

---

## Quickstart

```sh
git clone <this repo> party-line-pager && cd party-line-pager
./party-line-pager.sh
```

A menu comes up. Work down it, and it writes `config/policy.toml`,
`config/adapters.toml` and `.env` for you, refusing to start until the
requirements are actually met.

Two numbers to expect: **about five minutes** of configuration, then a
**20+ minute first build** you can walk away from. The build needs
roughly 8 GB of RAM available to Docker; machines with less may
need `CARGO_BUILD_JOBS=1` in the Dockerfile.

The same script is the admin console afterwards, so `./party-line-pager.sh` is the only
command worth memorising. Everything it does is a `docker compose` or
`party-line-pagerctl` invocation documented in the [full docs](docs/); the script
only saves the typing. It reads the config files at startup and owns them while
it runs, so to hand-edit them, quit it first.

---

## V1: all-in-one full image

Everything in one container, no Docker socket required. Tor, I2P,
Reticulum, Signal (native mode), and all chat adapters run as local
processes. A ttyd web terminal serves the `party-line-pager.sh` wizard at
port 7681. Image size is ~1.1GB.

### Build

```sh
docker build -f deploy/Dockerfile.full -t party-line-pager-full .
```

### Three operating modes

**1. Wizard mode** (first-time setup, or ongoing admin via browser):

```sh
docker run -d --name party-line-pager \
  -p 7681:7681 \
  -v ./config:/config \
  -e TTYD_CREDENTIAL=admin:changeme \
  party-line-pager-full
```

Opens a browser terminal at `http://<host>:7681`. The wizard walks you
through adapters, policy, and Signal linking. Config files are written to
the `/config` volume. No daemon runs until you tell the wizard to start it.

**2. Wizard + daemon** (admin terminal with the daemon already running):

```sh
docker run -d --name party-line-pager \
  -p 7681:7681 \
  -v ./config:/config \
  -e PLP_AUTO_START=1 \
  -e TTYD_CREDENTIAL=admin:changeme \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e PLP_PROVIDERS="tor,i2p,rns,web" \
  party-line-pager-full
```

The daemon starts in the background on boot, then ttyd opens the wizard.
Browse to `http://<host>:7681` to manage the running instance.

**3. Headless** (daemon only, no browser terminal):

```sh
docker run -d --name party-line-pager \
  -v ./config:/config \
  -e PLP_HEADLESS=1 \
  -e TELEGRAM_TOKEN="your-bot-token" \
  -e PLP_PROVIDERS="tor,i2p,rns,web" \
  party-line-pager-full
```

No ttyd, no wizard, no port 7681. The daemon runs as PID 1.
Administration is `docker exec` only:

```sh
docker exec party-line-pager party-line-pagerctl --state /config/state roster
docker exec party-line-pager party-line-pagerctl --state /config/state pause
```

### Verifying a deployment

```sh
# Logs: look for "party-line-pagerd is up" and "Listening on port: 7681"
docker logs party-line-pager

# Processes: tini, ttyd (if not headless), party-line-pagerd
docker exec party-line-pager ps aux

# Daemon responding:
docker exec party-line-pager party-line-pagerctl \
  --state /config/state roster
```

### Docker run syntax reminder

All options (`-e`, `--name`, `-p`, `-v`) go **before** the image name.
Docker treats the first bare word after the options as the image; anything
after it becomes the container's command.

```
docker run [OPTIONS] IMAGE
              ^        ^
              |        +-- always last
              +-- -d, --name, -p, -v, -e all go here
```

---

## V2: modular compose

Each transport runs in its own container, pulled from published images.
No git checkout needed; `docker compose up` against `transports/` works
out of the box. The provider hooks start and stop the transport containers
via the Docker socket.

Transport compose files in `transports/` set `command: ["relay"]` so
the upstream entrypoint enters relay mode without blocking on a
confirmation prompt. Without this, `docker compose up -d` (detached,
no TTY) hangs indefinitely on an open stdin pipe.

To use a local build instead of the published image, set the matching
env var in `.env` (e.g. `TOR_PARTYLINE_DIR=${PWD}/party-lines/tor-party-line`).

---

## CI/CD

Two GitHub Actions workflows, both manual (button in the Actions
tab, no auto-triggers):

- **CI** (`ci.yml`): runs tests and cargo-deny audit, then
  builds both images to confirm they compile.
- **Release** (`release.yml`): prompts for a version number and
  which registries to push (Docker Hub, GHCR, GitLab CR). Tests
  gate the push. A self-hosted registry placeholder is ready to
  uncomment.

A GitLab CI pipeline (`.gitlab-ci.yml`) mirrors the same flow.
All jobs are manual (click to run in the Pipelines UI).

Base images are pinned to exact versions. See
[Building and Testing](docs/development/building-testing.md#cicd)
for the full CI/CD reference: secrets setup, image names per
registry, and current version pins.

---

## Dependency management (Renovate)

A self-hosted [Renovate](https://docs.renovatebot.com/) instance
autodiscovers this repo and opens PRs for dependency updates.
`renovate.json` at the repo root holds the repo-specific config;
the global config lives in the separate `renovate` project
(`~/Projects/renovate/config.js`).

What Renovate manages here:

| Manager | Files | Notes |
|---------|-------|-------|
| `cargo` | `Cargo.toml`, `Cargo.lock` | 7-day `minimumReleaseAge` mirrors `cooldown.toml`; never auto-merged |
| `dockerfile` | `Dockerfile`, `deploy/Dockerfile.full` | Digest pin + tag updates |
| `docker-compose` | `compose/*.yml`, `transports/*.yml` | Self-published `marcusholtz/*` images disabled until on Docker Hub |
| `github-actions` | `.github/workflows/*.yml` | SHA-pinned action updates |

Ignored paths: `party-lines/` (upstream repos), `to-do/`.

After a Cargo PR from Renovate, run `hooks/audit-cargo-deps.sh`
before merging. Renovate does not run cargo-deny or cargo-cooldown
itself.

When your own images are published, remove the `marcusholtz/*`
package ignore rule from `renovate.json` to get digest updates for
transport images.

---

## Full documentation

The complete reference lives in [`docs/`](docs/):

| Section | What's there |
|---------|-------------|
| [Getting Started](docs/getting-started/) | Fat image (Unraid), compose quickstart, manual install |
| [Configuration](docs/configuration/policy.md) | policy.toml, adapters.toml, compose overlays, all 10 chat network setup guides |
| [Usage](docs/usage/commands.md) | Subscriber commands, admin CLI, upgrading |
| [Reference](docs/reference/provider-hooks.md) | Provider hooks, message rendering, state files, security model, supply chain |
| [Development](docs/development/building-testing.md) | Building, testing, CI/CD, design decisions, the party line trifecta |
| [Troubleshooting](docs/troubleshooting.md) | Common problems and fixes |
| [Roadmap](docs/roadmap.md) | What's planned, what's tabled |

Preview locally with the docs compose file (see
[docs/website/README.md](docs/website/README.md) for details):

```sh
docker compose -f docs/website/docker-compose.yml up serve
```

Then open `http://localhost:8000`.
