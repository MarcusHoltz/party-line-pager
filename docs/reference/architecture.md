# Architecture

PartyLinePager runs in two deployment modes. Both use the same
daemon, the same hooks, and the same configuration files. The
difference is how transport relays are managed.

## Standard (multi-container)

The default. Each transport relay runs in its own container,
pulled from Docker Hub. The daemon dispatches to them through
`docker compose`. You enable only the adapters and providers
you need via compose overlays.

## Full (all-in-one)

One container holds the daemon, all three transport runtimes,
the admin terminal, and every chat adapter. No Docker socket
required. The daemon manages transports as local processes
via PID files.

Best for environments that need a single `docker run` with no
compose tooling (Unraid, Portainer, appliance-style deploys).

## How they compare

``` mermaid
graph LR
  subgraph standard ["Standard (multi-container)"]
    direction TB
    D1[party-line-pagerd] -- hooks --> R1[plp-runtime.sh]
    R1 -- docker compose --> T1[Tor relay]
    R1 -- docker compose --> T2[I2P relay]
    R1 -- docker compose --> T3[RNS relay]
  end

  subgraph full ["Full (all-in-one)"]
    direction TB
    D2[party-line-pagerd] -- hooks --> R2[plp-runtime.sh]
    R2 -- PID files --> T4[Tor process]
    R2 -- PID files --> T5[I2P process]
    R2 -- PID files --> T6[RNS process]
    D2 -.- TT[ttyd admin terminal]
  end
```

Both modes share identical hook scripts. The runtime shim
(`plp-runtime.sh`) checks `PARTY_LINE_PAGER_RUNTIME` and
dispatches accordingly: `compose` for Standard, `direct` for
Full. See [provider hooks](provider-hooks.md#runtime-shim-standard-vs-full)
for the shim details.

### When to pick which

| | Standard | Full |
|---|---|---|
| Setup | compose overlays | single `docker run` |
| Transport isolation | separate containers, no port conflicts | shared network stack, one relay at a time |
| Docker socket | required for transport containers | not required |
| Admin access | host SSH / `party-line-pagerctl` | browser terminal (ttyd) |
| Image size | ~200 MB base + relay images | ~1.1 GB |
