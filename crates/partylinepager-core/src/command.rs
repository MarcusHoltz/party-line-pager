//! The user command parser.
//!
//! This is the only code path reachable by a stranger on any of the seven chat
//! networks, so it is deliberately tiny. Nine verbs exist and none of them are
//! privileged: every administrative action lives in `partylinepagerctl`, which is
//! reachable only over SSH. Anything unrecognized returns [`None`] and the
//! daemon stays silent rather than arguing in an IRC channel or a Mastodon
//! thread.
//!
//! Four of those verbs open a room, one per provider: `tor`, `i2p` and `rns`
//! for a party line on the matching network, `web` for a plain WebRTC room
//! URL. The parser names the verb and nothing more; whether an instance
//! actually offers that provider is a policy question, answered in the
//! engine.

use crate::config::ProviderKind;
use crate::quiet::QuietWindow;

/// Longest note we will carry into a broadcast.
pub const MAX_NOTE_LEN: usize = 200;

/// How many leading non-empty lines are examined before giving up. Email
/// clients put the command in the subject, in the body, or under a `Re:`
/// prefix, and IRC users prepend nicknames.
const MAX_LINES_SCANNED: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open a room of the named kind, with an optional one-line note for the
    /// invite. There is no bare, providerless form: every verb names its own
    /// provider, so opening a room always says which one it means.
    Open(ProviderKind, Option<String>),
    /// Tear down the room the sender opened, before it expires on its own.
    Close,
    Subscribe,
    Unsubscribe,
    /// Set the IANA timezone name. Validated later, against the tz database.
    Tz(String),
    Quiet(QuietArg),
    Status,
    /// Bare `help`, or `help <verb>` for one command in detail. The argument
    /// is the raw word the sender typed; whether it names a command this
    /// instance offers is a question for the renderer, which has the policy.
    Help(Option<String>),
    /// Deep-dive reference pages. Bare `wiki` lists topics, `wiki <topic>`
    /// shows the page.
    Wiki(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuietArg {
    /// Bare `quiet`: report the current window.
    Show,
    /// `quiet off`: clear it.
    Clear,
    /// `quiet 23:00-07:00`: set it.
    Set(QuietWindow),
}

/// Parses an inbound message, scanning the first few non-empty lines.
///
/// Returns `None` when nothing in those lines is a command, which the daemon
/// treats as "not for us".
pub fn parse(input: &str) -> Option<Command> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(MAX_LINES_SCANNED)
        .find_map(parse_line)
}

/// Parses exactly one line.
pub fn parse_line(line: &str) -> Option<Command> {
    let mut words = line
        .split_whitespace()
        // Drop leading @mentions so "@partylinepager@example.org signal" works.
        .skip_while(|w| w.starts_with('@'));

    let verb = words.next()?;
    // Strip common bot prefixes: /signal, !signal, .signal
    let verb = verb.trim_start_matches(['/', '!', '.']).to_ascii_lowercase();
    // Some clients append punctuation to a lone word.
    let verb = verb.trim_end_matches([',', ':', '!', '?', '.']);
    let rest = words.collect::<Vec<_>>().join(" ");

    match verb {
        // Explicit, one word per provider. No aliases on these two: the whole
        // point of naming them is that there is never a question about which
        // mechanism opened a room. The old generic words (`signal`, `raise`,
        // `batsignal`) that used to mean "whichever this instance has" are
        // gone; unrecognized now, same as any other word.
        "tor" => Some(Command::Open(ProviderKind::Tor, sanitize_note(&rest))),
        "i2p" => Some(Command::Open(ProviderKind::I2p, sanitize_note(&rest))),
        "rns" => Some(Command::Open(ProviderKind::Rns, sanitize_note(&rest))),
        "web" => Some(Command::Open(ProviderKind::Web, sanitize_note(&rest))),
        "close" => Some(Command::Close),
        "sub" | "subscribe" => Some(Command::Subscribe),
        "unsub" | "unsubscribe" | "stop" => Some(Command::Unsubscribe),
        "tz" | "timezone" => {
            let zone = rest.split_whitespace().next()?;
            Some(Command::Tz(zone.to_string()))
        }
        "quiet" => Some(Command::Quiet(parse_quiet_arg(&rest))),
        "status" => Some(Command::Status),
        "help" => Some(Command::Help(help_topic(&rest))),
        "wiki" | "guide" => Some(Command::Wiki(help_topic(&rest))),
        _ => None,
    }
}

/// The verb a `help` line asks about, normalized the same way a verb is.
fn help_topic(rest: &str) -> Option<String> {
    let word = rest.split_whitespace().next()?;
    let word = word
        .trim_start_matches(['/', '!', '.'])
        .trim_end_matches([',', ':', '!', '?', '.'])
        .to_ascii_lowercase();
    (!word.is_empty()).then_some(word)
}

fn parse_quiet_arg(rest: &str) -> QuietArg {
    let rest = rest.trim();
    if rest.is_empty() {
        return QuietArg::Show;
    }
    if rest.eq_ignore_ascii_case("off")
        || rest.eq_ignore_ascii_case("none")
        || rest.eq_ignore_ascii_case("clear")
    {
        return QuietArg::Clear;
    }
    match rest.parse::<QuietWindow>() {
        Ok(window) => QuietArg::Set(window),
        // An unparseable argument still asks a question, so show current state
        // and let the reply carry the expected syntax.
        Err(_) => QuietArg::Show,
    }
}

/// Flattens a note into something safe to paste into an outbound message:
/// control characters become spaces, runs of whitespace collapse, and the
/// result is truncated on a character boundary.
fn sanitize_note(raw: &str) -> Option<String> {
    let flattened: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_NOTE_LEN).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn tor(note: Option<&str>) -> Option<Command> {
        Some(Command::Open(ProviderKind::Tor, note.map(str::to_string)))
    }

    #[test]
    fn parses_every_verb() {
        assert_eq!(parse("tor"), tor(None));
        assert_eq!(
            parse("rns"),
            Some(Command::Open(ProviderKind::Rns, None))
        );
        assert_eq!(
            parse("web"),
            Some(Command::Open(ProviderKind::Web, None))
        );
        assert_eq!(parse("close"), Some(Command::Close));
        assert_eq!(parse("sub"), Some(Command::Subscribe));
        assert_eq!(parse("subscribe"), Some(Command::Subscribe));
        assert_eq!(parse("unsub"), Some(Command::Unsubscribe));
        assert_eq!(parse("stop"), Some(Command::Unsubscribe));
        assert_eq!(parse("status"), Some(Command::Status));
        assert_eq!(parse("help"), Some(Command::Help(None)));
        assert_eq!(parse("wiki"), Some(Command::Wiki(None)));
        assert_eq!(parse("guide"), Some(Command::Wiki(None)));
        assert_eq!(parse("tz America/Denver"), Some(Command::Tz("America/Denver".into())));
        assert_eq!(parse("quiet"), Some(Command::Quiet(QuietArg::Show)));
    }

    #[test]
    fn verbs_are_case_insensitive_and_accept_bot_prefixes() {
        assert_eq!(parse("/TOR"), tor(None));
        assert_eq!(parse("!tor"), tor(None));
        assert_eq!(
            parse("/Web"),
            Some(Command::Open(ProviderKind::Web, None))
        );
        assert_eq!(parse(".Help"), Some(Command::Help(None)));
    }

    #[test]
    fn the_old_generic_words_are_gone() {
        // signal/raise/batsignal used to mean "whichever this instance has".
        // Removed on purpose: the four provider verbs are the only way to open
        // a room now, so silence is the right answer, same as any unknown word.
        // partyline/reticulum are the old, longer spellings of tor/rns: also
        // gone, full removal rather than an alias, same as signal/raise/batsignal.
        for word in [
            "signal", "raise", "batsignal", "/SIGNAL", "!raise", "partyline", "reticulum",
        ] {
            assert_eq!(parse(word), None, "{word} should no longer be recognized");
        }
    }

    #[test]
    fn leading_mentions_are_ignored() {
        assert_eq!(
            parse("@partylinepager@example.org tor beers"),
            tor(Some("beers"))
        );
        assert_eq!(parse("@bot help"), Some(Command::Help(None)));
    }

    #[test]
    fn unknown_input_is_silence() {
        assert_eq!(parse("hello there"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("   \n  \n "), None);
        assert_eq!(parse("approve telegram:1"), None, "admin verbs must not exist");
        assert_eq!(parse("ban telegram:1"), None);
        assert_eq!(parse("pause"), None);
    }

    #[test]
    fn finds_a_command_in_a_later_line() {
        let email = "Re: your bot\n\n> quoted noise\ntor poker night\n";
        assert_eq!(parse(email), tor(Some("poker night")));
    }

    #[test]
    fn gives_up_after_five_non_empty_lines() {
        let mut body = String::new();
        for _ in 0..5 {
            body.push_str("quoted noise\n");
        }
        body.push_str("tor\n");
        assert_eq!(parse(&body), None);
    }

    #[test]
    fn notes_are_flattened_and_truncated() {
        assert_eq!(parse("web  a\tb   c "), {
            Some(Command::Open(ProviderKind::Web, Some("a b c".into())))
        });

        let long = format!("tor {}", "x".repeat(500));
        let Some(Command::Open(_, Some(note))) = parse(&long) else {
            panic!("expected a note");
        };
        assert_eq!(note.chars().count(), MAX_NOTE_LEN);
    }

    #[test]
    fn notes_cannot_inject_extra_lines() {
        // A note is pasted into an outbound message, so newlines must not survive.
        let Some(Command::Open(_, Some(note))) =
            parse_line("tor hi\u{0}\u{7}there\u{85}now")
        else {
            panic!("expected a note");
        };
        assert!(!note.chars().any(char::is_control), "got {note:?}");
        assert_eq!(note, "hi there now");
    }

    #[test]
    fn help_carries_the_verb_it_was_asked_about() {
        let topic = |word: &str| Some(Command::Help(Some(word.to_string())));
        assert_eq!(parse("help close"), topic("close"));
        assert_eq!(parse("help CLOSE!"), topic("close"));
        assert_eq!(parse("help /tz"), topic("tz"));
        // A word this instance does not offer still parses. The renderer
        // answers with the general list rather than confirming what exists.
        assert_eq!(parse("help banana"), topic("banana"));
        assert_eq!(parse("help close now"), topic("close"), "only the first word");
    }

    #[test]
    fn wiki_carries_the_topic_it_was_asked_about() {
        let topic = |word: &str| Some(Command::Wiki(Some(word.to_string())));
        assert_eq!(parse("wiki audio"), topic("audio"));
        assert_eq!(parse("guide audio"), topic("audio"));
        assert_eq!(parse("wiki AUDIO!"), topic("audio"));
        assert_eq!(parse("wiki unknown"), topic("unknown"));
    }

    #[test]
    fn quiet_accepts_set_show_and_clear() {
        assert_eq!(
            parse("quiet 23:00-07:00"),
            Some(Command::Quiet(QuietArg::Set(QuietWindow::new(
                NaiveTime::from_hms_opt(23, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(7, 0, 0).unwrap()
            ))))
        );
        assert_eq!(parse("quiet off"), Some(Command::Quiet(QuietArg::Clear)));
        assert_eq!(parse("quiet OFF"), Some(Command::Quiet(QuietArg::Clear)));
        assert_eq!(parse("quiet nonsense"), Some(Command::Quiet(QuietArg::Show)));
    }

    #[test]
    fn tz_without_an_argument_is_not_a_command() {
        assert_eq!(parse("tz"), None);
    }
}
