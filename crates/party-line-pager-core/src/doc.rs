//! How a message is marked up on the way out.
//!
//! [`render`](crate::render) builds a [`Doc`], a short list of blocks, and each
//! adapter turns that into whatever its own network accepts. The alternative,
//! one plain string for everybody, is what shipped first: it puts label columns
//! together with spaces, which survives in IRC and email and falls apart in the
//! five transports that render a proportional font. A subscriber on Telegram
//! saw the onion address and the shared secret as ragged text.
//!
//! Blocks carry meaning, not appearance. [`Block::Fields`] means "these only
//! make sense lined up", and it is each style's problem to decide whether that
//! is spaces, a fenced block or `<pre>`.

/// The markup a transport accepts.
///
/// Four rather than two, because the two HTML dialects genuinely disagree about
/// what a newline means and merging them would break one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// No markup at all: IRC, XMPP, Mastodon, Signal, email and Apprise take
    /// the text exactly as written. Alignment is done with spaces and only
    /// survives in a monospace client, which is the best that can be done.
    Plain,
    /// Markdown, as Discord and Mattermost both render a message body with no
    /// opt-in flag. Text inside a fenced block is literal, so the aligned
    /// blocks need no escaping; prose outside one does.
    Markdown,
    /// The tag subset Telegram accepts with `parse_mode=HTML`. Newlines in the
    /// source are honoured, `<p>` and `<br/>` are not in the subset, and an
    /// unbalanced tag is a 400 rather than a stray angle bracket.
    TelegramHtml,
    /// Matrix `formatted_body`. Real HTML, so a newline outside `<pre>`
    /// collapses to a space and block elements do the spacing instead.
    MatrixHtml,
}

/// One piece of a message, named for what it means rather than how it looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// The lead line. Bold wherever the transport has bold.
    Heading(String),
    /// A sentence or two of prose. The client is free to wrap it.
    Para(String),
    /// Label and value pairs that are only readable lined up: the credentials,
    /// the status report, the command list. Always monospace.
    Fields(Vec<(String, String)>),
    /// Verbatim text the reader is expected to copy. Monospace, and on most
    /// clients this is what makes it tap-to-copy on a phone.
    Code(String),
    /// Small print. Last, quieter than the rest, never load-bearing.
    Note(String),
}

/// A message, before anybody has decided what it looks like.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doc {
    blocks: Vec<Block>,
}

impl Doc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn heading(mut self, text: impl Into<String>) -> Self {
        self.blocks.push(Block::Heading(text.into()));
        self
    }

    pub fn para(mut self, text: impl Into<String>) -> Self {
        self.blocks.push(Block::Para(text.into()));
        self
    }

    pub fn code(mut self, text: impl Into<String>) -> Self {
        self.blocks.push(Block::Code(text.into()));
        self
    }

    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.blocks.push(Block::Note(text.into()));
        self
    }

    pub fn fields<K, V>(mut self, rows: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let rows: Vec<(String, String)> = rows
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        if !rows.is_empty() {
            self.blocks.push(Block::Fields(rows));
        }
        self
    }

    /// Appends another document's blocks. Used where one reply embeds another,
    /// such as `status` carrying the live room's credentials.
    pub fn append(mut self, other: Doc) -> Self {
        self.blocks.extend(other.blocks);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Plain text. Shorthand for the style the tests and the logs care about.
    pub fn plain(&self) -> String {
        self.render(Style::Plain)
    }

    pub fn render(&self, style: Style) -> String {
        match style {
            Style::Plain => self.render_plain(),
            Style::Markdown => self.render_markdown(),
            Style::TelegramHtml => self.render_html(false),
            Style::MatrixHtml => self.render_html(true),
        }
    }

    fn render_plain(&self) -> String {
        self.join(
            "\n\n",
            self.blocks.iter().map(|block| match block {
                Block::Heading(t) | Block::Para(t) | Block::Code(t) | Block::Note(t) => t.clone(),
                Block::Fields(rows) => align(rows),
            }),
        )
    }

    fn render_markdown(&self) -> String {
        self.join(
            "\n\n",
            self.blocks.iter().map(|block| match block {
                Block::Heading(t) => format!("**{}**", md(t)),
                Block::Para(t) => md(t),
                Block::Note(t) => format!("_{}_", md(t)),
                // A fence is literal, so what goes inside it is never escaped.
                // Escaping there would put backslashes in front of the reader.
                Block::Fields(rows) => format!("```\n{}\n```", align(rows)),
                Block::Code(t) => format!("`{t}`"),
            }),
        )
    }

    /// Both HTML dialects, split by the one thing they disagree on: Matrix
    /// needs block elements because its newlines mean nothing, Telegram needs
    /// newlines because it rejects the block elements.
    fn render_html(&self, blocks_are_elements: bool) -> String {
        let wrap = |inner: String| {
            if blocks_are_elements {
                format!("<p>{inner}</p>")
            } else {
                inner
            }
        };
        let line_break = |t: &str| {
            if blocks_are_elements {
                html(t).replace('\n', "<br/>")
            } else {
                html(t)
            }
        };

        let rendered = self.blocks.iter().map(|block| match block {
            Block::Heading(t) => wrap(format!("<b>{}</b>", line_break(t))),
            Block::Para(t) => wrap(line_break(t)),
            Block::Note(t) => wrap(format!("<i>{}</i>", line_break(t))),
            Block::Code(t) => wrap(format!("<code>{}</code>", html(t))),
            // <pre> preserves newlines in both dialects, which is the whole
            // reason the aligned blocks go in one.
            Block::Fields(rows) => format!("<pre>{}</pre>", html(&align(rows))),
        });

        self.join(if blocks_are_elements { "" } else { "\n\n" }, rendered)
    }

    fn join(&self, sep: &str, parts: impl Iterator<Item = String>) -> String {
        parts.collect::<Vec<_>>().join(sep)
    }
}

/// A bare string is a one-paragraph document.
///
/// Convenience for the places that genuinely have nothing to mark up: an
/// internal error reply, and the fixtures in the adapter tests.
impl From<&str> for Doc {
    fn from(text: &str) -> Self {
        Doc::new().para(text)
    }
}

impl From<String> for Doc {
    fn from(text: String) -> Self {
        Doc::new().para(text)
    }
}

/// Pads labels to a common width so the values start in one column.
///
/// Width is counted in `char`s rather than bytes. Every label we ship is
/// ASCII, but a label is one `format!` away from carrying a subscriber's
/// endpoint, and byte-counting a multi-byte label would silently misalign the
/// whole block.
fn align(rows: &[(String, String)]) -> String {
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|(label, value)| {
            let pad = " ".repeat(width - label.chars().count());
            // A value with its own newlines keeps them, indented under the
            // column it started in, rather than breaking the alignment.
            let value = value.replace('\n', &format!("\n{}", " ".repeat(width + 2)));
            format!("{label}{pad}  {value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Escapes the characters Discord and Mattermost treat as markup.
///
/// Deliberately minimal. Over-escaping is worse than under-escaping here: a
/// stray backslash in front of a full stop is visible to every reader, while
/// the unescaped characters left alone (`#`, `[`, `(`) only do anything in
/// positions our copy never puts them in. The set covered is the one a
/// subscriber's free-text note can actually reach.
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '_' | '~' | '`' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Doc {
        Doc::new()
            .heading("PartyLinePager: the room is open")
            .para("telegram:12345 opened the room.")
            .fields(vec![
                ("Onion", "batcave7xyz.onion"),
                ("Secret", "AAAABBBBCCCC"),
            ])
            .note("Reply unsub to stop these.")
    }

    #[test]
    fn plain_aligns_fields_into_one_column() {
        let text = creds().plain();
        assert!(text.contains("Onion   batcave7xyz.onion"), "{text}");
        assert!(text.contains("Secret  AAAABBBBCCCC"), "{text}");
    }

    #[test]
    fn markdown_fences_the_aligned_block_so_a_proportional_font_cannot_ruin_it() {
        let text = creds().render(Style::Markdown);
        assert!(text.contains("**PartyLinePager: the room is open**"), "{text}");
        assert!(text.contains("```\nOnion   batcave7xyz.onion"), "{text}");
        assert!(text.contains("_Reply unsub to stop these._"), "{text}");
    }

    #[test]
    fn telegram_html_keeps_newlines_and_uses_no_block_elements() {
        let text = creds().render(Style::TelegramHtml);
        assert!(text.contains("<b>PartyLinePager: the room is open</b>"), "{text}");
        assert!(text.contains("<pre>Onion   batcave7xyz.onion"), "{text}");
        assert!(!text.contains("<p>"), "telegram rejects <p>: {text}");
        assert!(!text.contains("<br/>"), "telegram rejects <br/>: {text}");
    }

    #[test]
    fn matrix_html_uses_block_elements_because_its_newlines_do_nothing() {
        let text = creds().render(Style::MatrixHtml);
        assert!(text.contains("<p><b>PartyLinePager: the room is open</b></p>"), "{text}");
        assert!(text.contains("<pre>Onion   batcave7xyz.onion"), "{text}");
    }

    #[test]
    fn a_note_cannot_smuggle_markup_into_any_style() {
        let doc = Doc::new().para("Note: *not bold* <b>not bold</b> & co");

        let markdown = doc.render(Style::Markdown);
        assert!(markdown.contains(r"\*not bold\*"), "{markdown}");

        for style in [Style::TelegramHtml, Style::MatrixHtml] {
            let html = doc.render(style);
            assert!(html.contains("&lt;b&gt;not bold&lt;/b&gt;"), "{html}");
            assert!(html.contains("&amp; co"), "{html}");
        }
    }

    #[test]
    fn fenced_content_is_never_escaped() {
        // A fence is literal. Escaping inside one would show the reader a
        // backslash in the middle of a secret they are about to copy.
        let doc = Doc::new().fields(vec![("Secret", "a_b*c")]);
        assert!(doc.render(Style::Markdown).contains("a_b*c"));
    }

    #[test]
    fn a_multi_line_value_stays_inside_its_column() {
        let doc = Doc::new().fields(vec![("Quota", "spent\nresets in 2h")]);
        assert_eq!(doc.plain(), "Quota  spent\n       resets in 2h");
    }

    #[test]
    fn an_empty_field_list_adds_no_block() {
        assert!(Doc::new().fields(Vec::<(String, String)>::new()).is_empty());
    }
}
