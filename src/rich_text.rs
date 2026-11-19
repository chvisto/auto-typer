use std::{any::TypeId, borrow::Cow, cell::RefCell, ops::Range};

use eframe::egui::{
    Color32, FontFamily, FontId, TextBuffer,
    text::{CharIndex, LayoutJob, TextFormat},
};
use html5ever::{
    LocalName,
    tokenizer::{
        BufferQueue, CharacterTokens, EndTag, StartTag, TagToken, Token, TokenSink,
        TokenSinkResult, Tokenizer,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Default)]
pub struct Document {
    text: String,
    styles: Vec<Style>,
    paste: Option<(String, Vec<Style>)>,
}

impl std::ops::Deref for Document {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl Document {
    pub fn styles(&self) -> &[Style] {
        &self.styles
    }

    pub fn has_styles(&self) -> bool {
        self.styles.iter().any(|style| *style != Style::default())
    }

    pub fn prepare_paste(&mut self, plain: &str, html: &str) {
        let plain = normalize(plain);
        if let Some(styles) = formats(&plain, html) {
            self.paste = Some((plain.into_owned(), styles));
        }
    }

    pub fn clear_pending_paste(&mut self) {
        self.paste = None;
    }

    pub fn layout(&self, size: f32, sent: usize, show_styles: bool) -> LayoutJob {
        let mut job = LayoutJob::default();
        let mut start = 0;
        let mut format = None;
        for (index, (byte, _)) in self.text.char_indices().enumerate() {
            let style = if show_styles {
                self.styles[index]
            } else {
                Style::default()
            };
            let next = (style, index < sent);
            if format != Some(next) {
                if let Some(previous) = format {
                    append(&mut job, &self.text[start..byte], size, previous);
                }
                start = byte;
                format = Some(next);
            }
        }
        if let Some(previous) = format {
            append(&mut job, &self.text[start..], size, previous);
        }
        job
    }
}

fn append(job: &mut LayoutJob, text: &str, size: f32, (style, sent): (Style, bool)) {
    let family = if style.bold {
        FontFamily::Name(super::SEMIBOLD.into())
    } else {
        FontFamily::Proportional
    };
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: FontId::new(size, family),
            italics: style.italic,
            color: if sent { Color32::WHITE } else { super::MUTED },
            line_height: (size == 19.0).then_some(29.0),
            ..Default::default()
        },
    );
}

impl TextBuffer for Document {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn insert_text(&mut self, text: &str, char_index: CharIndex) -> usize {
        let text = normalize(text);
        let index = char_index.0.min(self.styles.len());
        let count = self.text.insert_text(&text, CharIndex(index));
        if let Some((_, styles)) = self
            .paste
            .take_if(|(plain, _)| plain.as_str() == text.as_ref())
        {
            self.styles.splice(index..index, styles);
        } else {
            let inherited = index
                .checked_sub(1)
                .and_then(|left| self.styles.get(left))
                .filter(|style| self.styles.get(index) == Some(*style))
                .copied()
                .unwrap_or_default();
            self.styles
                .splice(index..index, std::iter::repeat_n(inherited, count));
        }
        count
    }

    fn delete_char_range(&mut self, range: Range<CharIndex>) {
        let len = self.styles.len();
        self.styles
            .drain(range.start.0.min(len)..range.end.0.min(len));
        self.text.delete_char_range(range);
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}

fn normalize(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

#[derive(Default)]
struct Fragment {
    parents: Vec<(LocalName, Style, bool)>,
    visible: Vec<(char, Style)>,
    has_fragments: bool,
    in_fragment: bool,
}

#[derive(Default)]
struct FragmentSink(RefCell<Fragment>);

fn is_ignored(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
}

impl TokenSink for FragmentSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Fragment {
            parents,
            visible,
            has_fragments,
            in_fragment,
        } = &mut *self.0.borrow_mut();
        let (mut style, mut hidden) = parents
            .last()
            .map_or((Style::default(), false), |p| (p.1, p.2));
        match token {
            CharacterTokens(text) if !hidden && (!*has_fragments || *in_fragment) => {
                visible.extend(
                    text.chars()
                        .filter(|ch| !is_ignored(*ch))
                        .map(|ch| (ch, style)),
                );
            }
            Token::CommentToken(comment) => match comment.trim() {
                "StartFragment" => {
                    if !*has_fragments {
                        visible.clear();
                        *has_fragments = true;
                    }
                    *in_fragment = true;
                }
                "EndFragment" if *has_fragments => {
                    *in_fragment = false;
                }
                _ => {}
            },
            TagToken(tag) if tag.kind == StartTag => {
                let name = tag.name.as_ref();
                style.bold |= matches!(
                    name,
                    "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th"
                );
                style.italic |= matches!(name, "i" | "em" | "cite" | "dfn" | "var");
                hidden |= matches!(name, "head" | "script" | "style" | "template")
                    || tag.attrs.iter().any(|a| &*a.name.local == "hidden");
                if let Some(attribute) = tag.attrs.iter().find(|a| &*a.name.local == "style") {
                    for declaration in attribute.value.split(';') {
                        let Some((key, value)) = declaration.split_once(':') else {
                            continue;
                        };
                        let value = value
                            .split('!')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_ascii_lowercase();
                        match key.trim().to_ascii_lowercase().as_str() {
                            "display" if value == "none" => hidden = true,
                            "visibility" if matches!(value.as_str(), "hidden" | "collapse") => {
                                hidden = true
                            }
                            "font-weight" => match value.as_str() {
                                "bold" | "bolder" => style.bold = true,
                                "normal" | "lighter" => style.bold = false,
                                _ => {
                                    if let Ok(weight) = value.parse::<u16>() {
                                        style.bold = weight >= 600;
                                    }
                                }
                            },
                            "font-style" => {
                                if value == "normal" {
                                    style.italic = false;
                                } else if value == "italic" || value.starts_with("oblique") {
                                    style.italic = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if !tag.self_closing
                    && !matches!(
                        name,
                        "area"
                            | "base"
                            | "br"
                            | "col"
                            | "embed"
                            | "hr"
                            | "img"
                            | "input"
                            | "link"
                            | "meta"
                            | "param"
                            | "source"
                            | "track"
                            | "wbr"
                    )
                {
                    parents.push((tag.name, style, hidden));
                }
            }
            TagToken(tag) if tag.kind == EndTag => {
                if let Some(index) = parents.iter().rposition(|parent| parent.0 == tag.name) {
                    parents.truncate(index);
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn formats(plain: &str, html: &str) -> Option<Vec<Style>> {
    let tokenizer = Tokenizer::new(FragmentSink::default(), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let rich = tokenizer.sink.0.into_inner().visible;
    if rich.is_empty() {
        return None;
    }
    let mut ri = 0;
    let mut matched = 0usize;
    let mut total = 0usize;
    let mut last = Style::default();
    let mut styles = Vec::with_capacity(plain.chars().count());
    for ch in plain.chars() {
        if !is_ignored(ch) {
            total += 1;
            if let Some(&(actual, style)) = rich.get(ri)
                && actual == ch
            {
                last = style;
                ri += 1;
                matched += 1;
            } else if let Some(offset) = rich[ri..].iter().take(4).position(|&(c, _)| c == ch) {
                ri += offset;
                last = rich[ri].1;
                ri += 1;
                matched += 1;
            }
        }
        styles.push(last);
    }
    (matched > 0 && matched * 5 >= total * 4 && matched * 5 >= rich.len() * 4).then_some(styles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_styles_follow_selection_replacement_and_unicode_edits() {
        let mut doc = Document::default();
        let text = "Café bold & both\r\nitalic end";
        let html = r#"<b style="font-weight:normal">Café <strong>bold &amp; <em>both</em></strong><br><span style="font-style:italic">italic</span> end</b>"#;
        doc.insert_text("first last", CharIndex(0));
        doc.delete_char_range(CharIndex(6)..CharIndex(10));
        doc.prepare_paste(text, html);
        doc.insert_text(text, CharIndex(6));
        assert_eq!(doc.as_str(), "first Café bold & both\nitalic end");
        assert_eq!(
            doc.styles()[11],
            Style {
                bold: true,
                italic: false
            }
        );
        assert_eq!(
            doc.styles()[18],
            Style {
                bold: true,
                italic: true
            }
        );
        assert_eq!(
            doc.styles()[23],
            Style {
                bold: false,
                italic: true
            }
        );
        doc.delete_char_range(CharIndex(6)..CharIndex(11));
        assert!(doc.styles()[6].bold);
        assert_eq!(doc.styles().len(), doc.chars().count());
        assert_eq!(doc.layout(14.0, usize::MAX, true).text, doc.as_str());
    }

    #[test]
    fn different_clipboard_text_does_not_inherit_html_styles() {
        assert!(formats("Something else", "<b>bold</b>").is_none());
        let plain = "Pace matters";
        let styles = formats(plain, "<span style='font-weight:700'>Pace</span> matters").unwrap();
        assert!(styles[0].bold);
        assert!(!styles[5].bold);
    }

    #[test]
    fn fragment_markers_zero_width_hidden_and_lists_preserve_styles() {
        let plain = "Bold\u{200B} and italic";
        let html = concat!(
            "<html><body>Ignored chrome<b>",
            "<!--StartFragment-->",
            "<span style='display:none'>secret</span>",
            "Bold and <span style='font-style:italic'>italic</span>",
            "<!--EndFragment-->",
            "</b>Trailing</body></html>"
        );
        let styles = formats(plain, html).unwrap();
        assert!(styles[0].bold);
        assert!(!styles[0].italic);
        assert!(styles[10].bold);
        assert!(styles[10].italic);

        let list_plain = "• Bold item\n• Italic item";
        let list_html = "<ul><li><b>Bold</b> item</li><li><i>Italic</i> item</li></ul>";
        let list_styles = formats(list_plain, list_html).unwrap();
        assert!(list_styles[2].bold);
        assert!(!list_styles[7].bold);
        assert!(list_styles[14].italic);
    }
}
