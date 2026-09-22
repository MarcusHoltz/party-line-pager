---
title: PartylinePager
---

<div class="plp-hero">

<img class="plp-hero-logo" src="website/assets/logo.svg" alt="PartylinePager pager logo">

<h1>PartylinePager</h1>

<p>Open a room on one chat network, and everyone who subscribed
on <strong>any</strong> chat network gets a way into it.</p>

<a class="md-button md-button--primary" href="getting-started/">Get started</a>
<a class="md-button" href="configuration/policy/">Configuration</a>

</div>

```
  Telegram   -+                                      +- tor ---------> tor-party-line
  Matrix     -|                                      |      (.onion)
  Signal     -|                                      +- i2p ---------> i2p-party-line
  IRC        -+-> adapters --> engine --> policy --> |      (.b32.i2p)
  XMPP       -|                  |                   +- rns ----------> reticulum-party-line
  Mastodon   -|                  |                   |      (32 char hash)
  Email      -|                  |                   +- web ---------> room URL
  Mattermost -|                  +                   |      (no secret)
  Discord    -+                  ^                   |
                         partylinepagerctl           +- fanout ------> message subscribers
                                                              (minus quiet hours)

                                         
                                 
```

Four kinds of room, one command each:

| Command | Network | What you get |
|------|---------|--------------|
| `tor` | Tor hidden services | Fresh `.onion` address + shared secret. Maximum privacy, 1-3 min startup. |
| `i2p` | I2P garlic routing | Fresh `.b32.i2p` address + shared secret. ~23 sec startup, no port forwarding. |
| `rns` | Reticulum mesh | Fresh destination hash + shared secret. Fastest, NAT-piercing. Not onion routing. |
| `web` | WebRTC (MiroTalk/Jitsi) | Room URL, no secret. Instant, works in any browser. |

Nine chat networks have native adapters. Everything else
[Apprise](https://github.com/caronc/apprise) can reach
(130+ services) works by adding an endpoint by hand.

---

## Pick your path

!!! tip "Unraid users / one-container setup"

    The **full image** puts everything in one container: 
    daemon, transports, browser-based admin terminal.
    No Docker socket needed.
    
    **[Get started with the full image](getting-started/unraid.md)**

!!! note "Docker Compose users"

    The core **standard setup** uses one base file plus one overlay per
    adapter and provider. Nothing starts that you didn't ask for.

    **[Get started with Docker Compose](getting-started/quickstart.md)**

!!! abstract "Manual / advanced"

    Skip the wizard. Hand-edit `policy.toml` and `adapters.toml`,
    pick your compose overlays, and bring it up yourself.

    **[Manual install](getting-started/manual-install.md)**

---

## The Trifecta

PartylinePager orchestrates three sibling projects, each a
standalone encrypted push-to-talk voice app built for a
different transport:


| | | |
|---|---|---|
| [![Tor Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--tor-onion-router-overlay-network.jpg)](https://gitlab.com/MarcusHoltz/tor-party-line) | [![I2P Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--invisible-internet-project-i2p-garlic-roter.jpg)](https://gitlab.com/MarcusHoltz/i2p-party-line) | [![Reticulum Party Line](https://raw.githubusercontent.com/MarcusHoltz/marcusholtz.github.io/refs/heads/main/assets/img/header/header--partyline--reticulum-network-stack.jpg)](https://gitlab.com/MarcusHoltz/reticulum-party-line) |
| [Tor Party Line](https://gitlab.com/MarcusHoltz/tor-party-line) | [I2P Party Line](https://gitlab.com/MarcusHoltz/i2p-party-line) | [Reticulum Party Line](https://gitlab.com/MarcusHoltz/reticulum-party-line) |



Each ships in triplicate: same TUI, same encryption, same PTT
semantics, different wire. PartylinePager opens a room on any of
them (or a WebRTC link), fans the credentials to your roster
across nine chat networks, and tears it down when the timer
runs out.

---
