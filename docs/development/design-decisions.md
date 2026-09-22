# Design Decisions

Recorded here because they were arguments, not defaults.

## Endpoint, not person

Quotas and tiers hang off an address. Linking identities across
networks would need a verification flow and would still be
guesswork; the admin polices abuse instead.

## Rolling quotas

A trimmed log of recent room timestamps per subscriber:
"max_rooms since the oldest one still in the window". No
calendar boundaries, no midnight herd. A tier that allows 2
rooms per 24h lets them land back to back rather than forcing
them 12h apart.

## Hard reject on quota, not a queue

A rejection states the exact wait and does nothing else.

## Nothing administrative over chat

Prevents a whole class of bug by removing the code path rather
than guarding it.

## Fire and forget

No occupancy tracking, no catch-up messages, no "party over"
notice. Every one of those is a metadata trail about who was
where.

## A live room short-circuits an open

If somebody opens while a room is up, they get the existing
credentials and spend no quota. This is provider-blind: a `web`
room while a party line is up returns the party line, and vice
versa. One room per instance, not one room per kind.

## Two explicit commands instead of one overloaded one

`tor` and `web` are different enough (one has a secret and
takes minutes, the other is a bare link and is instant) that a
single command with a flag would mean somebody eventually gets the
wrong kind of room without noticing.

Two commands also make "restrict this instance to one kind" fall
out of configuration presence: no enable flag, no toggle to
drift out of sync.

The old `signal`, `raise` and `batsignal` words are gone rather
than kept as aliases: with named commands an alias reintroduces the
ambiguity the rename exists to remove.

## Quota is shared between providers

A `web` room is nearly free to open, so there's an argument for
a cheaper quota. Not implemented: a quota here limits how often
somebody may summon a roomful of people, and that cost is paid
by the recipients either way.

## A teardown only acts on the room that is live

Timers outlive their rooms: an admin closes early, somebody
opens again, and the old timer fires against a party that is no
longer theirs to end.

## The backlog is not replayed

Telegram holds unconfirmed updates for about a day and Mastodon
keeps notifications indefinitely, so both adapters confirm
whatever accumulated while the daemon was down without acting on
it. A `tor` from last night must not summon people to a party
nobody is at.

## A failed hook costs nothing

Quota is stamped only after a room actually comes up. A broken
backend must not cost somebody their week.

## Adapters deliver their own outbound

The original plan sent everything through Apprise. Apprise has
no IRC plugin and its XMPP support depends on an optional
Python library, so native delivery through the session the
adapter already holds is both more reliable and more capable.
Apprise remains for the 130+ services with no adapter.

## Four networks skip their SDK

Telegram, Mastodon, Signal and Mattermost are plain REST.
IRC, XMPP, Matrix and IMAP use real libraries because those
protocols are genuinely hard.

`teloxide` and `megalodon` are good libraries, but not worth
the dependency weight in a daemon exposed to strangers.

## Matterbridge was rejected

Last push December 2024. Its REST API is channel-and-gateway
shaped with no per-user direct messages. Wrong shape for
subscriber fanout.
