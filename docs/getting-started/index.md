# Getting Started

Three ways to run PartylinePager. Pick one.

## Recomended for (Unraid, Portainer, etc)

The **full image** puts everything in one container: the daemon,
all three transport runtimes (Tor, I2P, Reticulum), Signal
(native mode, no JVM), a browser-based admin terminal, and all
nine chat adapters. No Docker socket required.

One `docker run`, a browser tab for the wizard, and you're
live.

**[Full image setup](unraid.md)**

---

## Docker Compose (with wizard)

One base file plus one overlay per adapter and provider you
actually run. Nothing starts that you didn't ask for, and a
credential you don't use never enters the daemon's environment.

Best for: operators who want fine-grained control over what
runs, or who already have compose workflows.

**[Compose quickstart](quickstart.md)**

---

## Manual install (no wizard)

Hand-edit `config/policy.toml` and `config/adapters.toml`, pick
your compose overlays, and bring it up yourself. Everything the
wizard does is a compose or `partylinepagerctl` invocation
documented in the reference.

Best for: anyone who wants to understand every moving part
before it moves.

**[Manual install](manual-install.md)**
