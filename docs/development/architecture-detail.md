# Architecture Detail

How the pieces connect, for contributors and anyone vetting the
internals. See [Architecture](../reference/architecture.md) for
the high-level comparison of deployment modes.

## Component layers

``` mermaid
graph TB
  subgraph adapters ["Chat Adapters"]
    A1[Telegram]
    A2[Matrix]
    A3[Signal]
    A4[IRC]
    A5[XMPP]
    A6[Mastodon]
    A7[Email]
    A8[Mattermost]
    A9[Discord]
    A10[Apprise]
  end

  subgraph daemon ["Daemon"]
    CMD[Command parser]
    ROSTER[Roster + quotas]
    RENDER[Message renderer]
  end

  subgraph hooks ["Hook Layer"]
    PH["provider-{tor,i2p,rns,web}.sh"]
    TH["teardown-{tor,i2p,rns,web}.sh"]
    SHIM[plp-runtime.sh]
  end

  subgraph transports ["Transport Relays"]
    TOR[tor-party-line]
    I2P[i2p-party-line]
    RNS[reticulum-party-line]
  end

  adapters --> CMD
  CMD --> ROSTER
  ROSTER --> PH
  PH --> SHIM
  SHIM --> transports
  transports -- address JSON --> PH
  PH -- address JSON --> RENDER
  RENDER --> adapters
  ROSTER --> TH
  TH --> SHIM
```

## Message flow

A room open follows this sequence:

``` mermaid
sequenceDiagram
  participant Sub as Subscriber
  participant Adapter as Chat Adapter
  participant Daemon as partylinepagerd
  participant Hook as provider hook
  participant Shim as plp-runtime.sh
  participant Relay as Transport relay

  Sub->>Adapter: "tor" / "web" / "i2p" / "rns"
  Adapter->>Daemon: parsed command
  Daemon->>Daemon: validate, check quota/tier
  Daemon->>Daemon: generate 256-bit secret (CSPRNG)
  Daemon->>Hook: call hook, secret on stdin
  Hook->>Shim: up <dir> <service>
  alt Standard (compose)
    Shim->>Relay: docker compose up -d
  else Full (direct)
    Shim->>Relay: start process, write PID file
  end
  Relay-->>Hook: readiness signal + address
  Hook-->>Daemon: {"address":"...","ttl_secs":N}
  Daemon->>Daemon: validate address format
  Daemon->>Daemon: render per-recipient (timezone, format)
  Daemon->>Adapter: broadcast message
  Adapter->>Sub: delivered (HTML / Markdown / plain)
```

## Runtime shim branching

`plp-runtime.sh` is the single dispatch point for all transport
lifecycle operations. Every provider hook calls it instead of
`docker compose` directly.

``` mermaid
flowchart TD
  HOOK[Provider hook] --> SHIM{plp-runtime.sh}
  SHIM --> CHECK{PARTYLINEPAGER_RUNTIME?}

  CHECK -- compose --> COMPOSE_UP["docker compose up -d"]
  CHECK -- compose --> COMPOSE_DOWN["docker compose down"]
  CHECK -- compose --> COMPOSE_EXEC["docker compose exec"]
  CHECK -- compose --> COMPOSE_LOGS["docker compose logs"]

  CHECK -- direct --> PID_START["Start process, write PID"]
  CHECK -- direct --> PID_STOP["Read PID, collect tree, kill"]
  CHECK -- direct --> DIRECT_EXEC["Run command in-process"]
  CHECK -- direct --> DIRECT_LOGS["Tail log file"]
```

In compose mode, `PLP_COMPOSE_ARGS` carries extra flags (override
files, profiles). In direct mode, the shim manages processes
through PID files under `$PLP_PID_DIR` and collects child process
trees up to 5 levels deep for clean shutdown.

## State and configuration

No database. The daemon reads and writes flat files:

| File | Purpose |
|---|---|
| `config/policy.toml` | Tiers, providers, quotas, room TTL |
| `config/adapters.toml` | Chat network credentials |
| `config/state/subscribers.json` | Roster: addresses, tiers, notes |
| `config/state/pending.json` | Pending signup confirmations |
| `config/state/room.json` | Currently live room (if any) |
| `config/state/runtime.json` | Daemon PID, startup timestamp |
| `.env` | UIDs, compose file list, runtime overrides |

All state files are re-read on each relevant operation. No
in-memory cache outlives a single request cycle.
