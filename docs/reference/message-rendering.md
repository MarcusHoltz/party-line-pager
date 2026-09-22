# Message Rendering

`render.rs` writes every subscriber-facing string once, as a
`Doc`: a short list of blocks that say what a piece of a message
*means* (a heading, prose, a set of label/value fields, verbatim
text to copy, small print). Each adapter then marks that `Doc`
up for its own network. One piece of copy, nine renderings.

## Why this exists

Credentials and the `status` report are only readable lined up
in columns. The first version built those columns out of spaces
and sent the identical string everywhere. That is correct in
IRC and email and falls apart in every client with a
proportional font: subscribers on Telegram were reading a
ragged onion address and shared secret.

## Rendering styles

| Style | Networks | What it does |
|---|---|---|
| Markdown | Discord, Mattermost | `**bold**` headings; aligned blocks inside a fence (tap-to-copy) |
| Telegram HTML | Telegram | `parse_mode=HTML`, `<b>` and `<pre>` |
| Matrix HTML | Matrix | `formatted_body`, plain body as fallback |
| Plain | IRC, XMPP, Mastodon, Signal, email, Apprise | No markup, columns aligned with spaces |

### Why Telegram uses HTML, not MarkdownV2

MarkdownV2 requires escaping 18 characters and rejects the
whole message with a 400 if one is missed. A subscriber note
containing a full stop would lose the broadcast. HTML mode
needs three characters escaped.

### Why Telegram and Matrix can't share an HTML renderer

Telegram honours newlines and rejects `<p>` and `<br/>`.
Matrix is real HTML, where a newline outside `<pre>` collapses
to a space. Separate styles for that one reason.

### Why XMPP and Signal stay plain

XEP-0393 message styling and signal-cli-rest-api's
`text_mode: styled` are advisory and version-dependent, with no
way to detect support. A mismatch ships the markers as literal
text.

## Escaping

A subscriber's free-text note is escaped for whichever style it
lands in, so a note cannot smuggle markup into anyone's client.
Text inside a fenced or `<pre>` block is never escaped, because
a fence is already literal and escaping would put backslashes
in the middle of a secret somebody is about to copy.

## Per-recipient times

The closing time in a broadcast is shown in each subscriber's
own timezone (set with `tz`). Subscribers with no `tz` set get
UTC, stated as UTC.

This is why the invite is built once per recipient rather than
once per room. The cost is one small allocation per subscriber,
against a network call.
