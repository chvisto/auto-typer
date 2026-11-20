use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use encoding_rs::{Encoding, WINDOWS_1252};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OPENFILENAMEW,
    },
};

use crate::{
    docx,
    rich_text::{Document, Heading, Style, heading_kind, normalize},
};

const IMPORT_LIMIT: u64 = 16 * 1024 * 1024;

pub fn choose(owner: HWND) -> Result<Option<PathBuf>, String> {
    let mut path = vec![0u16; 32768];
    let filter: Vec<u16> = "Supported files (*.doc; *.docx; *.txt)\0*.doc;*.docx;*.txt\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "Open file\0".encode_utf16().collect();
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
            return Err("Could not open the file picker.".into());
        }
        return Ok(None);
    }
    let end = path.iter().position(|&ch| ch == 0).unwrap_or(path.len());
    Ok(Some(PathBuf::from(OsString::from_wide(&path[..end]))))
}

pub fn read(path: &Path) -> Result<Document, String> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("docx") {
        return docx::read(path);
    }
    let legacy = extension.eq_ignore_ascii_case("doc");
    if !legacy && !extension.eq_ignore_ascii_case("txt") {
        return Err("Choose a .doc, .docx or .txt file.".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(IMPORT_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read the selected file.")?;
    if bytes.len() as u64 > IMPORT_LIMIT {
        return Err("The file exceeds the 16 MB import limit.".into());
    }
    let document = if legacy {
        let source = rwml::Document::open(&bytes)
            .map_err(|error| format!("Could not open this Word 97–2003 file. {error}"))?;
        let mut document = Document::default();
        append_legacy_blocks(&mut document, &source.model().blocks, &mut false);
        document
    } else {
        decode_text(&bytes)?
    };
    if document.trim().is_empty() {
        return Err("The file has no text to import.".into());
    }
    Ok(document)
}

fn decode_text(bytes: &[u8]) -> Result<Document, String> {
    let text = if let Some((encoding, length)) = Encoding::for_bom(bytes) {
        let (text, malformed) = encoding.decode_without_bom_handling(&bytes[length..]);
        if malformed {
            return Err("The text file contains invalid characters for its encoding.".into());
        }
        text
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        text.into()
    } else {
        WINDOWS_1252.decode_without_bom_handling(bytes).0
    };
    if text.contains('\0') {
        return Err(
            "Choose a plain text file saved as UTF-8, UTF-16 with a BOM or Windows ANSI.".into(),
        );
    }
    let mut document = Document::default();
    document.push(&normalize(&text), Style::default());
    Ok(document)
}

fn append_legacy_blocks(document: &mut Document, blocks: &[rwml::Block], seen: &mut bool) {
    for block in blocks {
        match block {
            rwml::Block::Paragraph(paragraph) => {
                if *seen {
                    document.push("\n", Style::default());
                }
                *seen = true;
                let heading = paragraph
                    .props
                    .style_name
                    .as_deref()
                    .and_then(heading_kind)
                    .or_else(|| {
                        paragraph
                            .props
                            .heading_level
                            .filter(|level| (1..=9).contains(level))
                            .map(Heading::Level)
                    })
                    .unwrap_or_default();
                for run in paragraph.runs.iter().filter(|run| !run.props.hidden) {
                    document.push(
                        &normalize(&run.text),
                        Style {
                            bold: run.props.bold,
                            italic: run.props.italic,
                            heading,
                        },
                    );
                }
            }
            rwml::Block::Table(table) => {
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    append_legacy_blocks(document, &cell.blocks, seen);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_import_decodes_a_bom_and_normalizes_line_endings() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(
            "Café\r\nSecond line\rThird"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        let document = decode_text(&bytes).unwrap();
        assert_eq!(&*document, "Café\nSecond line\nThird");
        assert!(!document.has_styles());
        assert_eq!(&*decode_text(b"caf\xe9").unwrap(), "café");
    }
}
