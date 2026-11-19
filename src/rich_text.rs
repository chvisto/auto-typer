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
            let style = if show_styles { self.styles[index] } else { Style::default() };
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
        if let Some((_, styles)) = self.paste.take_if(|(plain, _)| plain.as_str() == text.as_ref()) {
            self.styles.splice(index..index, styles);
        } else {
            let inherited = index.checked_sub(1)
                .and_then(|left| self.styles.get(left))
                .filter(|style| self.styles.get(index) == Some(*style))
                .copied()
                .unwrap_or_default();
            self.styles.splice(index..index, std::iter::repeat_n(inherited, count));
        }
        count
    }

    fn delete_char_range(&mut self, range: Range<CharIndex>) {
        let len = self.styles.len();
        self.styles.drain(range.start.0.min(len)..range.end.0.min(len));
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
}

#[derive(Default)]
struct FragmentSink(RefCell<Fragment>);

impl TokenSink for FragmentSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Fragment { parents, visible } = &mut *self.0.borrow_mut();
        let (mut style, hidden) = parents.last().map_or((Style::default(), false), |p| (p.1, p.2));
        match token {
            CharacterTokens(text) if !hidden => {
                visible.extend(text.chars().filter(|ch| !ch.is_whitespace()).map(|ch| (ch, style)));
            }
            TagToken(tag) if tag.kind == StartTag => {
                let name = tag.name.as_ref();
                style.bold |= matches!(name, "b" | "strong");
                style.italic |= matches!(name, "i" | "em");
                if let Some(attribute) = tag.attrs.iter().find(|a| a.name.local.as_ref() == "style") {
                    for declaration in attribute.value.split(';') {
                        let Some((key, value)) = declaration.split_once(':') else { continue };
                        let value = value.split('!').next().unwrap_or_default().trim().to_ascii_lowercase();
                        match key.trim().to_ascii_lowercase().as_str() {
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
                if !tag.self_closing && !matches!(
                    name, "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input"
                        | "link" | "meta" | "param" | "source" | "track" | "wbr"
                ) {
                    let hidden = hidden || matches!(name, "head" | "script" | "style" | "template");
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
    let mut rich = tokenizer.sink.0.into_inner().visible.into_iter();
    let mut last = Style::default();
    let mut styles = Vec::with_capacity(plain.chars().count());
    for ch in plain.chars() {
        if !ch.is_whitespace() {
            let (actual, style) = rich.next()?;
            if actual != ch {
                return None;
            }
            last = style;
        }
        styles.push(last);
    }
    rich.next().is_none().then_some(styles)
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
        assert_eq!(doc.styles()[11], Style { bold: true, italic: false });
        assert_eq!(doc.styles()[18], Style { bold: true, italic: true });
        assert_eq!(doc.styles()[23], Style { bold: false, italic: true });
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
}
