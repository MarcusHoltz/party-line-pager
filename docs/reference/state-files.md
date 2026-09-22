# State Files

No database, and **no Docker named volumes**. JSON files in
`./config/state`, written with write-to-temp plus atomic rename,
mutated under an exclusive `flock`. `jq` works on all of it,
from the host, with no container in the way.

## Files

| File | Contents |
|---|---|
| `subscribers.json` | The roster, including pending and banned entries. |
| `pending.json` | Requests held for approval. |
| `room.json` | The live room. Absent means nothing is up. |
| `runtime.json` | Pause switch, close request. |
| `.lock` | The advisory lock both binaries take. |

## Everything lives in this directory

Every persistent thing the deployment has is a file here.
A named volume is invisible to `tar`, easy to forget, and turns
"move to another machine" into an archaeology exercise.

| Directory | Holds | Replaceable? |
|---|---|---|
| `state/` | Roster, quotas, live room, pause switch | **No.** Irreplaceable. |
| `signal-cli/` | Signal linked-device identity | No, without re-registering |
| `tor-party-line/` | Party line checkout + Tor data | Yes (git clone) |
| `.cache/`, `target/` | Cargo registry and build artifacts | Yes (regenerated on build) |

`state/` and `signal-cli/` are in `.gitignore`: they travel
with the directory, not with the repository.

## Container UIDs

The containers run as `HOST_UID:HOST_GID` from `.env`, so every
file they write belongs to you rather than root. Without those
set, the containers run as root and you need `sudo` to read
your own roster. See `.env.example`.

## Moving to another machine

```sh
docker compose down
tar czf partylinepager.tar.gz \
    --exclude=./.cache --exclude=./target \
    --exclude=./.git .
# copy to new host, unpack, then:
printf 'HOST_UID=%s\nHOST_GID=%s\nDOCKER_GID=%s\n' \
    "$(id -u)" "$(id -g)" \
    "$(getent group docker | cut -d: -f3)"
# update .env with the output above
docker compose up -d
docker compose exec partylinepagerd partylinepagerctl who
```

The two excludes are build caches. The `.env` UIDs are the only
per-machine value: a stale one shows up as permission errors on
`state/`.

Nothing else is left behind except rebuildable Docker images.

## Identity model

A subscriber **is an address on a service**. There is no person
model and no account linking: `telegram:123` and
`email:partylinepager@example.org` are two unrelated subscribers even if
the same human owns both. They get two quotas. Policing that is
the admin's job. There is no way to verify a human identity
across networks, and pretending otherwise would be theatre.
