# Administration

`partylinepagerctl` runs on the host, over SSH, as a user who
can read the state directory. It takes the same lock the daemon
takes, so an admin decision cannot interleave with a fanout in
progress. The daemon notices decisions within five seconds
without a restart.

Every command below is also a menu item in `./partylinepager.sh`,
which lists the roster first and lets you pick by number
instead of typing an endpoint id.

## Roster commands

```sh
partylinepagerctl who [--status pending|active|banned]
partylinepagerctl status
```

`who` and `pending` print a table with a header row, columns
sized to their contents:

```
STATUS  TIER     ENDPOINT                          TZ  QUIET  ROOMS   LAST ROOM
active  trusted  apprise:ntfy://ntfy.sh/plp        -   -      0       never
active  weekly   telegram:120646099                -   -      0       never
```

If you script against this, skip the header line and split on
runs of two or more spaces.

## Subscriber management

```sh
partylinepagerctl approve telegram:123
partylinepagerctl deny telegram:123
partylinepagerctl ban telegram:123
partylinepagerctl unban telegram:123
partylinepagerctl tier telegram:123 trusted
partylinepagerctl reset-quota telegram:123
```

Approving sends a note to the subscriber on their network the
moment it takes effect.

### Adding endpoints by hand

```sh
partylinepagerctl add apprise:ntfy://ntfy.sh/plp --tier trusted
partylinepagerctl add apprise:mailto://user:pw@smtp.example.org
```

This is the only way to add `apprise:` endpoints.

## Request management

```sh
partylinepagerctl pending
partylinepagerctl approve-request <id>
partylinepagerctl deny-request <id>
```

## Instance control

```sh
partylinepagerctl pause      # refuse new rooms instance-wide
partylinepagerctl resume
partylinepagerctl close      # tear the live room down early
```

## Common workflows

### Onboarding a new subscriber

They message the bot `sub`, landing as `pending` on
`default_tier`:

```sh
partylinepagerctl who --status pending
partylinepagerctl tier telegram:123 lurker  # optional
partylinepagerctl approve telegram:123
```

`tier` and `approve` are independent: `tier` edits
`subscribers.json` with no check on status, so it works before
or after `approve`. There is no single "approve onto tier X"
command.

### Wrong kind of room opened

Closing the room is only half the fix:

```sh
partylinepagerctl status     # host: line names who to reset
partylinepagerctl close      # torn down within 5 seconds
partylinepagerctl reset-quota telegram:123
```

`close` does not refund quota. Quota is spent at the moment a
room comes up, so on a `weekly` tier they would wait 168 hours
for a replacement without `reset-quota`.

Anyone *else* who tried while it was live got "The line is
already up" and needs no reset.

If the request is still held (never provisioned), use
`deny-request` instead. No room came up, no quota spent.

`./partylinepager.sh` does this sequence as **Rooms > Clear the
board**, reading the host off `status` automatically.
