//! Markdown from the doctor's agent, turned into pieces the panel can lay
//! out one below the other: paragraphs of styled text, code, pictures.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// How a run of text looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
}

/// A run of text that looks the same throughout, and where it links to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub text: String,
    pub marks: Marks,
    pub link: Option<String>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Span {
        Span { text: text.into(), marks: Marks::default(), link: None }
    }
}

/// What a paragraph of text is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Paragraph,
    Heading(u8),
    /// A list item, `depth` lists deep. The marker ("•", "2.") is empty for
    /// the item's later paragraphs.
    Item {
        depth: u8,
        marker: String,
    },
    Quote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text {
        kind: Kind,
        spans: Vec<Span>,
    },
    /// A code block and its language ("bash", or empty when not given).
    Code {
        lang: String,
        text: String,
    },
    Image {
        url: String,
        alt: String,
        link: Option<String>,
    },
    Rule,
}

/// The pictures in `source`, by address, in order.
pub fn images(source: &str) -> Vec<String> {
    parse(source)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Image { url, .. } => Some(url),
            _ => None,
        })
        .collect()
}

pub fn parse(source: &str) -> Vec<Piece> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    builder.flush();
    builder.pieces
}

/// The command a code block runs, when it is shell: its text without the
/// `$ ` prompts (and, when there are prompts, without the output between).
/// `None` for other languages; a block without one counts as shell.
pub fn shell_command(lang: &str, text: &str) -> Option<String> {
    let shell = ["", "sh", "bash", "zsh", "shell", "console", "terminal", "shellsession"];
    if !shell.contains(&lang.to_ascii_lowercase().as_str()) || text.trim().is_empty() {
        return None;
    }
    let prompted: Vec<&str> = text.lines().filter_map(|line| line.trim_start().strip_prefix("$ ")).collect();
    Some(if prompted.is_empty() { text.to_string() } else { prompted.join("\n") })
}

#[derive(Default)]
struct Builder {
    pieces: Vec<Piece>,
    spans: Vec<Span>,
    bold: u8,
    italic: u8,
    strike: u8,
    link: Option<String>,
    /// The next number of each list we are in; `None` for bullets.
    lists: Vec<Option<u64>>,
    /// The marker of the item whose first text is still to come.
    marker: Option<String>,
    quotes: u8,
    heading: Option<u8>,
    /// The code block being read: language and text so far.
    code: Option<(String, String)>,
    /// The picture being read: address and its description so far.
    image: Option<(String, String)>,
    table_head: bool,
}

impl Builder {
    fn marks(&self) -> Marks {
        Marks { bold: self.bold > 0 || self.table_head, italic: self.italic > 0, code: false, strike: self.strike > 0 }
    }

    fn push(&mut self, text: &str, marks: Marks) {
        if text.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.marks == marks && last.link == self.link => last.text.push_str(text),
            _ => self.spans.push(Span { text: text.into(), marks, link: self.link.clone() }),
        }
    }

    /// Ends the text so far as a piece of its own.
    fn flush(&mut self) {
        // Trailing spaces and line breaks would only add empty room.
        while let Some(last) = self.spans.last_mut() {
            let trimmed = last.text.trim_end().len();
            last.text.truncate(trimmed);
            if !last.text.is_empty() {
                break;
            }
            self.spans.pop();
        }
        if self.spans.iter().all(|span| span.text.trim().is_empty()) {
            self.spans.clear();
            return;
        }
        let kind = if let Some(level) = self.heading {
            Kind::Heading(level)
        } else if !self.lists.is_empty() {
            Kind::Item { depth: (self.lists.len() - 1) as u8, marker: self.marker.take().unwrap_or_default() }
        } else if self.quotes > 0 {
            Kind::Quote
        } else {
            Kind::Paragraph
        };
        self.pieces.push(Piece::Text { kind, spans: std::mem::take(&mut self.spans) });
    }

    fn event(&mut self, event: Event) {
        if let Some((_, alt)) = &mut self.image {
            match event {
                Event::Text(text) | Event::Code(text) => alt.push_str(&text),
                Event::End(TagEnd::Image) => {
                    let (url, alt) = self.image.take().unwrap_or_default();
                    self.flush();
                    self.pieces.push(Piece::Image { url, alt, link: self.link.clone() });
                }
                _ => {}
            }
            return;
        }
        if let Some((_, code)) = &mut self.code {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, code) = self.code.take().unwrap_or_default();
                    self.pieces.push(Piece::Code { lang, text: code.trim_end_matches('\n').into() });
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push(&text, self.marks()),
            Event::Code(text) => self.push(&text, Marks { code: true, ..self.marks() }),
            Event::SoftBreak => self.push(" ", self.marks()),
            Event::HardBreak => self.push("\n", self.marks()),
            Event::Html(html) | Event::InlineHtml(html) => {
                let tag = html.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                    self.push("\n", self.marks());
                } else {
                    self.push(&html, self.marks());
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push(&text, Marks { code: true, ..self.marks() })
            }
            Event::Rule => {
                self.flush();
                self.pieces.push(Piece::Rule);
            }
            Event::TaskListMarker(done) => self.marker = Some(if done { "\u{2611}" } else { "\u{2610}" }.into()),
            Event::FootnoteReference(_) => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quotes += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or_default().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(first) => {
                self.flush();
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}.", *number - 1)
                    }
                    _ => "\u{2022}".into(),
                };
                self.marker = Some(marker);
            }
            Tag::Table(_) => self.flush(),
            Tag::TableHead => self.table_head = true,
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.into_string()),
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.into_string(), String::new())),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            // A table as lines of cells.
            TagEnd::TableCell => self.push("    ", self.marks()),
            TagEnd::TableHead => {
                self.table_head = false;
                self.push("\n", self.marks());
            }
            TagEnd::TableRow => self.push("\n", self.marks()),
            TagEnd::Table => self.flush(),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(piece: &Piece) -> String {
        match piece {
            Piece::Text { spans, .. } => spans.iter().map(|span| span.text.as_str()).collect(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn inline_marks_and_links() {
        let pieces = parse("Some **bold** and `code` with [a link](https://example.com).");
        let [Piece::Text { kind: Kind::Paragraph, spans }] = pieces.as_slice() else { panic!("{pieces:?}") };
        assert!(spans.iter().any(|span| span.text == "bold" && span.marks.bold));
        assert!(spans.iter().any(|span| span.text == "code" && span.marks.code));
        assert!(spans.iter().any(|span| span.text == "a link" && span.link.as_deref() == Some("https://example.com")));
    }

    #[test]
    fn lists_number_their_items() {
        let pieces = parse("1. one\n2. two\n   - nested\n");
        let kinds: Vec<_> = pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { kind: Kind::Item { depth, marker }, .. } => (*depth, marker.clone(), text(piece)),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [(0, "1.".into(), "one".into()), (0, "2.".into(), "two".into()), (1, "\u{2022}".into(), "nested".into())]
        );
    }

    #[test]
    fn images_split_paragraphs() {
        let pieces = parse("**Simple Raccoon**\n![Simple Raccoon wallpaper](https://example.com/a.png)\n");
        assert_eq!(text(&pieces[0]), "Simple Raccoon");
        assert_eq!(
            pieces[1],
            Piece::Image {
                url: "https://example.com/a.png".into(),
                alt: "Simple Raccoon wallpaper".into(),
                link: None
            }
        );
        assert_eq!(images("![x](https://example.com/b.jpg)"), ["https://example.com/b.jpg"]);
    }

    #[test]
    fn code_blocks_keep_their_lines() {
        let pieces = parse("```toml\na = 1\nb = 2\n```\n");
        assert_eq!(pieces, [Piece::Code { lang: "toml".into(), text: "a = 1\nb = 2".into() }]);
    }

    #[test]
    fn shell_blocks_run_without_prompts() {
        assert_eq!(shell_command("bash", "sudo pacman -Syu").as_deref(), Some("sudo pacman -Syu"));
        assert_eq!(shell_command("", "ls").as_deref(), Some("ls"));
        assert_eq!(
            shell_command("console", "$ sudo systemctl restart foo\nok\n$ echo hi\nhi").as_deref(),
            Some("sudo systemctl restart foo\necho hi")
        );
        assert_eq!(shell_command("toml", "a = 1"), None);
        assert_eq!(shell_command("sh", "  \n"), None);
    }
}
