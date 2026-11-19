use std::{collections::HashMap, ffi::OsString, fs::File, io::Read, os::windows::ffi::OsStringExt, path::{Path, PathBuf}};

use roxmltree::{Document as Xml, Node};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::Controls::Dialogs::{CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OPENFILENAMEW},
};
use zip::{ZipArchive, result::ZipError};

use crate::rich_text::{Document, Heading, Style, heading_kind};

const XML_LIMIT: u64 = 16 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 128 * 1024 * 1024;
const WORDML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const STRICT_WORDML: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";

pub fn choose(owner: HWND) -> Result<Option<PathBuf>, String> {
    let mut path = vec![0u16; 32768];
    let filter: Vec<u16> = "Word documents (*.docx)\0*.docx\0\0".encode_utf16().collect();
    let title: Vec<u16> = "Import Word document\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: the owner is the app's HWND and all dialog buffers live through the call.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        if unsafe { CommDlgExtendedError() } != 0 {
            return Err("Could not open the Word document picker.".into());
        }
        return Ok(None);
    }
    let end = path.iter().position(|&ch| ch == 0).unwrap_or(path.len());
    Ok(Some(PathBuf::from(OsString::from_wide(&path[..end]))))
}

pub fn read(path: &Path) -> Result<Document, String> {
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("docx")) {
        return Err("Choose a Word .docx document.".into());
    }
    let file = File::open(path).map_err(|_| "Could not open the Word document.")?;
    if file.metadata().map_err(|_| "Could not read the Word document.")?.len() > ARCHIVE_LIMIT {
        return Err("The Word document exceeds 128 MB.".into());
    }
    let mut archive = ZipArchive::new(file)
        .map_err(|_| "The file is not a readable Word .docx document.")?;
    let main = part(&mut archive, "word/document.xml")?
        .ok_or("The file is missing the Word document body.")?;
    let styles = part(&mut archive, "word/styles.xml")?;
    parse(&main, styles.as_deref())
}

fn part(archive: &mut ZipArchive<File>, name: &str) -> Result<Option<String>, String> {
    let part = match archive.by_name(name) {
        Ok(part) => part,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err("Could not read part of the Word document.".into()),
    };
    if part.size() > XML_LIMIT {
        return Err("The Word document contains too much text to import.".into());
    }
    let mut xml = String::new();
    part.take(XML_LIMIT + 1).read_to_string(&mut xml)
        .map_err(|_| "The Word document contains unreadable text.")?;
    if xml.len() as u64 > XML_LIMIT {
        return Err("The Word document contains too much text to import.".into());
    }
    Ok(Some(xml))
}

fn word(node: Node<'_, '_>, name: &str) -> bool {
    let tag = node.tag_name();
    tag.name() == name && matches!(tag.namespace(), Some(WORDML | STRICT_WORDML))
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|&node| word(node, name))
}

fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute((WORDML, name)).or_else(|| node.attribute((STRICT_WORDML, name)))
}

fn val<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    attr(child(node, name)?, "val")
}

fn enabled(value: &str) -> bool {
    !matches!(value, "0" | "false" | "off")
}

fn inline(style: &mut Style, props: Node<'_, '_>, inherited: bool) {
    for (state, name) in [(&mut style.bold, "b"), (&mut style.italic, "i")] {
        if let Some(node) = child(props, name) {
            let on = attr(node, "val").is_none_or(enabled);
            *state = if inherited { *state ^ on } else { on };
        }
    }
}

fn parse(main: &str, styles: Option<&str>) -> Result<Document, String> {
    let main = Xml::parse(main).map_err(|_| "The Word document body is invalid.")?;
    let styles = styles.map(Xml::parse).transpose()
        .map_err(|_| "The Word document styles are invalid.")?;
    let definitions: HashMap<_, _> = styles.as_ref().into_iter()
        .flat_map(|xml| xml.root_element().children())
        .filter(|&node| word(node, "style"))
        .filter_map(|node| Some((attr(node, "styleId")?, node)))
        .collect();
    let default = definitions.values().copied().find(|&style| {
        attr(style, "type") == Some("paragraph") && attr(style, "default").is_some_and(enabled)
    });
    let doc_defaults = styles.as_ref()
        .and_then(|xml| child(xml.root_element(), "docDefaults"))
        .and_then(|node| child(node, "rPrDefault"))
        .and_then(|node| child(node, "rPr"));
    let body = child(main.root_element(), "body").ok_or("The file is missing the Word document body.")?;
    let mut document = Document::default();
    let mut first = true;
    let visible = |node: Node<'_, '_>| !node.ancestors().any(|node| {
        word(node, "del") || word(node, "moveFrom") || word(node, "txbxContent")
    });
    for paragraph in body.descendants().filter(|&node| word(node, "p") && visible(node)) {
        if !first {
            document.push("\n", Style::default());
        }
        first = false;
        let props = child(paragraph, "pPr");
        let style_id = props.and_then(|node| val(node, "pStyle"));
        let style = style_id.and_then(|id| definitions.get(id).copied()).or(default);
        let levels = |node| val(node, "outlineLvl").and_then(|n| n.parse::<u8>().ok())
            .filter(|&n| n < 9).map(|n| Heading::Level(n + 1));
        let resolve = |start| {
            let mut chain = Vec::new();
            let mut next = start;
            while let Some(node) = next {
                if chain.contains(&node) {
                    break;
                }
                chain.push(node);
                next = val(node, "basedOn").and_then(|id| definitions.get(id).copied());
            }
            chain
        };
        let chain = resolve(style);
        let named = |node| val(node, "name").and_then(heading_kind)
            .or_else(|| attr(node, "styleId").and_then(heading_kind))
            .or_else(|| child(node, "pPr").and_then(levels));
        let heading = props.and_then(levels)
            .or_else(|| style_id.and_then(heading_kind))
            .or_else(|| chain.iter().copied().find_map(named))
            .unwrap_or_default();
        let mut base = Style { heading, ..Default::default() };
        if heading == Heading::Body {
            if let Some(props) = doc_defaults {
                inline(&mut base, props, false);
            }
            for props in chain.into_iter().rev().filter_map(|node| child(node, "rPr")) {
                inline(&mut base, props, true);
            }
        }
        for run in paragraph.descendants().filter(|&node| word(node, "r") && visible(node)) {
            let mut style = base;
            if let Some(props) = child(run, "rPr") {
                let character = val(props, "rStyle").and_then(|id| definitions.get(id).copied());
                for props in resolve(character).into_iter().rev().filter_map(|node| child(node, "rPr")) {
                    inline(&mut style, props, true);
                }
                inline(&mut style, props, false);
            }
            for node in run.children() {
                let text = if word(node, "t") {
                    node.text().unwrap_or_default()
                } else if word(node, "tab") {
                    "\t"
                } else if word(node, "br") || word(node, "cr") {
                    "\n"
                } else if word(node, "noBreakHyphen") {
                    "\u{2011}"
                } else {
                    ""
                };
                document.push(text, style);
            }
        }
    }
    if document.trim().is_empty() {
        return Err("The Word document has no text to import.".into());
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_named_and_inherited_headings_and_inline_runs() {
        let main = format!(r#"<w:document xmlns:w="{WORDML}"><w:body>
            <w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Custom"/></w:pPr><w:r><w:t>Heading</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:rStyle w:val="Both"/><w:i w:val="off"/></w:rPr><w:t>Bold</w:t></w:r><w:r><w:tab/><w:t>ordinary</w:t><w:br/></w:r><w:r><w:rPr><w:rStyle w:val="Both"/></w:rPr><w:t>Café</w:t></w:r></w:p>
        </w:body></w:document>"#);
        let styles = format!(r#"<w:styles xmlns:w="{WORDML}">
            <w:style w:type="paragraph" w:styleId="Custom"><w:basedOn w:val="Heading9"/></w:style>
            <w:style w:type="paragraph" w:styleId="Heading9"><w:name w:val="heading 9"/></w:style>
            <w:style w:type="character" w:styleId="Both"><w:rPr><w:b/><w:i/></w:rPr></w:style>
        </w:styles>"#);
        let doc = parse(&main, Some(&styles)).unwrap();
        assert!(&*doc == "Title\nHeading\nBold\tordinary\nCafé");
        assert_eq!(doc.styles()[0].heading, Heading::Title);
        assert_eq!(doc.styles()[6].heading, Heading::Level(9));
        assert!(doc.styles()[14].bold && !doc.styles()[14].italic);
        assert_eq!(doc.styles()[19], Style::default());
        let last = doc.styles().last().unwrap();
        assert!(last.bold && last.italic);
    }
}
