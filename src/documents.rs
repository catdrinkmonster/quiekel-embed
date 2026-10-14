//! Office documents as plain text: Microsoft Office (also its macro, template and slide-show
//! variants), Office 97-2003, OpenDocument (LibreOffice, OpenOffice) and RTF.

use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::{NsReader, Reader};
use regex::Regex;

use crate::config::{self, suffix};
use crate::extract::{MAX_CHARS, OLE_MAGIC, ReadError, ReadResult, decode_text, strip_html};

const MAX_ROWS: usize = 5000; // spreadsheets: rows read per file
const MAX_PART_BYTES: u64 = 256 * 1024 * 1024; // one part of a document, unpacked

const ODF_EXTS: [&str; 12] = [".odt", ".ott", ".fodt", ".ods", ".ots", ".fods", ".odp", ".otp", ".fodp", ".odg", ".otg", ".fodg"];
const EXCEL_EXTS: [&str; 4] = [".xlsx", ".xlsm", ".xltx", ".xltm"];
const POWERPOINT_EXTS: [&str; 6] = [".pptx", ".pptm", ".ppsx", ".ppsm", ".potx", ".potm"];

/// The text of an office document or RTF file, whatever its exact flavour.
pub fn read(path: &Path) -> ReadResult<String> {
    let ext = suffix(&path.to_string_lossy());
    let head = crate::extract::head(path, 8).unwrap_or_default();
    if head.starts_with(OLE_MAGIC) {
        return read_ole(path); // Office 97-2003, or a password-protected modern Office file
    }
    if head.starts_with(b"{\\rtf") {
        return read_rtf(path); // (Word also saves RTF under .doc)
    }
    if head.starts_with(b"PK") {
        // By its name, or else by what's inside (a file under another name).
        let inside = || -> Vec<String> {
            open_zip(path).map(|z| z.file_names().map(String::from).collect()).unwrap_or_default()
        };
        let known = ODF_EXTS.contains(&ext.as_str()) || EXCEL_EXTS.contains(&ext.as_str()) || POWERPOINT_EXTS.contains(&ext.as_str());
        let names = if known { Vec::new() } else { inside() };
        let has = |prefix: &str| names.iter().any(|n| n.starts_with(prefix));
        if ODF_EXTS.contains(&ext.as_str()) || (has("content.xml") && has("mimetype")) {
            return read_odf(path);
        }
        if EXCEL_EXTS.contains(&ext.as_str()) || has("xl/") {
            return read_spreadsheet(path, false);
        }
        if POWERPOINT_EXTS.contains(&ext.as_str()) || has("ppt/") {
            return read_pptx(path);
        }
        return read_ooxml(path); // a .docx, or a modern Word file under another name
    }
    if [".fodt", ".fods", ".fodp", ".fodg"].contains(&ext.as_str()) {
        return read_odf(path);
    }
    // A web page or plain text saved under an Office name (old Excel "web page" files, ...)
    let data = std::fs::read(path)?;
    let text = decode_text(&data[..data.len().min(config::MAX_TEXT_BYTES as usize)])?;
    Ok(if text.trim_start().starts_with('<') { strip_html(&text) } else { text })
}

// ---- ZIP parts --------------------------------------------------------------------------

type Zip = zip::ZipArchive<std::fs::File>;

fn open_zip(path: &Path) -> ReadResult<Zip> {
    zip::ZipArchive::new(std::fs::File::open(path)?).map_err(|_| ReadError::Damaged("document"))
}

/// One part of the document, as bytes (limited: a ZIP can lie about sizes).
fn part(z: &mut Zip, name: &str) -> ReadResult<Option<Vec<u8>>> {
    let Ok(f) = z.by_name(name) else { return Ok(None) };
    let mut out = Vec::new();
    f.take(MAX_PART_BYTES + 1).read_to_end(&mut out).map_err(|_| ReadError::Damaged("document"))?;
    if out.len() as u64 > MAX_PART_BYTES {
        return Err(ReadError::Skip("too_large"));
    }
    Ok(Some(out))
}

fn names(z: &Zip) -> Vec<String> {
    z.file_names().map(str::to_string).collect()
}

fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key || local(a.key.as_ref()) == key).map(|a| {
        let raw = String::from_utf8_lossy(&a.value);
        quick_xml::escape::unescape(&raw).map(|v| v.into_owned()).unwrap_or_else(|_| raw.into_owned())
    })
}

fn xml_text(t: &quick_xml::events::BytesText) -> String {
    // (references like &amp; come as events of their own)
    t.decode().map(|s| s.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(t).into_owned())
}

// ---- Microsoft Office ---------------------------------------------------------------------

/// A Word, Excel or PowerPoint file (2007 and later), told apart by what's inside.
fn read_ooxml(path: &Path) -> ReadResult<String> {
    let z = open_zip(path)?;
    let n = names(&z);
    if n.iter().any(|x| x == "word/document.xml") {
        return read_docx(path);
    }
    if n.iter().any(|x| x == "xl/workbook.xml") {
        return read_spreadsheet(path, false);
    }
    if n.iter().any(|x| x == "ppt/presentation.xml") {
        return read_pptx(path);
    }
    Err(ReadError::Damaged("document"))
}

/// Word: the body's paragraphs, then its tables row by row ("a | b").
fn read_docx(path: &Path) -> ReadResult<String> {
    let mut z = open_zip(path)?;
    let xml = part(&mut z, "word/document.xml")?.ok_or(ReadError::Damaged("document"))?;
    let mut r = Reader::from_reader(xml.as_slice());
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut tables: Vec<String> = Vec::new(); // rows, "a | b"
    let mut para: Option<String> = None; // the paragraph being read (body level, or a body table's cell)
    let mut in_cell = false;
    let mut cell_paras: Vec<String> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut skip = 0usize; // inside drawings, text boxes, field codes, deleted text
    let mut in_t = false;
    let mut buf = Vec::new();
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|_| ReadError::Damaged("document"))?;
        match ev {
            Event::Start(e) => {
                let name = local(e.name().as_ref()).to_vec();
                let parent = stack.last().map(Vec::as_slice);
                let grand = stack.len().checked_sub(2).map(|i| stack[i].as_slice());
                match name.as_slice() {
                    b"drawing" | b"pict" | b"AlternateContent" | b"txbxContent" | b"delText" | b"instrText" | b"object" => skip += 1,
                    b"p" if skip == 0 && parent == Some(b"body") => para = Some(String::new()),
                    b"p" if skip == 0 && parent == Some(b"tc") && in_cell => para = Some(String::new()),
                    b"tc" if parent == Some(b"tr") && grand == Some(b"tbl") && stack.len() == 4 => {
                        in_cell = true;
                        cell_paras.clear();
                    }
                    b"tr" if parent == Some(b"tbl") && stack.len() == 3 => row.clear(),
                    b"t" => in_t = true,
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                if let Some(p) = para.as_mut() {
                    if skip == 0 {
                        match local(e.name().as_ref()) {
                            b"tab" | b"ptab" => p.push('\t'),
                            b"br" | b"cr" => p.push('\n'),
                            b"noBreakHyphen" => p.push('-'),
                            _ => {}
                        }
                    }
                }
            }
            Event::Text(t) => {
                if in_t && skip == 0 {
                    if let Some(p) = para.as_mut() {
                        p.push_str(&xml_text(&t));
                    }
                }
            }
            Event::GeneralRef(rf) => {
                if in_t && skip == 0 {
                    if let Some(p) = para.as_mut() {
                        if let Ok(Some(c)) = rf.resolve_char_ref() {
                            p.push(c);
                        } else {
                            let name = String::from_utf8_lossy(&rf);
                            p.push_str(match name.as_ref() { "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", _ => "" });
                        }
                    }
                }
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let parent = stack.last().map(Vec::as_slice);
                match name.as_slice() {
                    b"drawing" | b"pict" | b"AlternateContent" | b"txbxContent" | b"delText" | b"instrText" | b"object" => {
                        skip = skip.saturating_sub(1)
                    }
                    b"t" => in_t = false,
                    b"p" if para.is_some() && (parent == Some(b"body") || parent == Some(b"tc")) => {
                        let text = para.take().unwrap_or_default();
                        if parent == Some(b"body") {
                            paragraphs.push(text);
                        } else {
                            cell_paras.push(text);
                        }
                    }
                    b"tc" if in_cell && parent == Some(b"tr") && stack.len() == 4 => {
                        in_cell = false;
                        row.push(cell_paras.join("\n").trim().to_string());
                    }
                    b"tr" if parent == Some(b"tbl") && stack.len() == 3 => tables.push(row.join(" | ")),
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
        if paragraphs.iter().map(String::len).sum::<usize>() > 2 * MAX_CHARS {
            break;
        }
    }
    paragraphs.extend(tables);
    Ok(paragraphs.join("\n"))
}

/// The targets of a part's relationships, by id.
fn rels(z: &mut Zip, path: &str) -> ReadResult<Vec<(String, String, String)>> {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let rels_path = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
    let Some(xml) = part(z, &rels_path)? else { return Ok(vec![]) };
    let mut r = Reader::from_reader(xml.as_slice());
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"Relationship" => {
                let id = attr(&e, b"Id").unwrap_or_default();
                let target = attr(&e, b"Target").unwrap_or_default();
                let kind = attr(&e, b"Type").unwrap_or_default();
                out.push((id, resolve_target(dir, &target), kind));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn resolve_target(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The text of a shape tree's top-level shapes: text frames' paragraphs, and tables row by row.
fn shapes_text(xml: &[u8], only_body_placeholder: bool) -> Vec<String> {
    let mut r = Reader::from_reader(xml);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut out = Vec::new();
    let mut paras: Vec<String> = Vec::new(); // the current text frame's paragraphs
    let mut para: Option<String> = None;
    let mut in_t = false;
    let mut shape_depth: Option<usize> = None; // the top-level shape being read
    let mut is_body = false;
    let mut table_rows: Vec<String> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell: Option<Vec<String>> = None;
    let mut buf = Vec::new();
    loop {
        let ev = match r.read_event_into(&mut buf) {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => {
                let name = local(e.name().as_ref()).to_vec();
                // top-level shapes: children of the slide's spTree
                if stack.last().map(|s| s.as_slice()) == Some(b"spTree") && shape_depth.is_none() {
                    if name.as_slice() == b"sp" || name.as_slice() == b"graphicFrame" {
                        shape_depth = Some(stack.len());
                        is_body = false;
                        paras.clear();
                        table_rows.clear();
                    }
                }
                match name.as_slice() {
                    b"p" if shape_depth.is_some() => para = Some(String::new()),
                    b"t" => in_t = true,
                    b"tc" if shape_depth.is_some() => cell = Some(Vec::new()),
                    b"tr" if shape_depth.is_some() => row.clear(),
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == b"br" {
                    if let Some(p) = para.as_mut() {
                        p.push('\u{b}');
                    }
                }
                if name == b"ph" && shape_depth.is_some() {
                    is_body = attr(&e, b"type").as_deref() == Some("body");
                }
            }
            Event::Text(t) => {
                if in_t {
                    if let Some(p) = para.as_mut() {
                        p.push_str(&xml_text(&t));
                    }
                }
            }
            Event::GeneralRef(rf) => {
                if in_t {
                    if let Some(p) = para.as_mut() {
                        if let Ok(Some(c)) = rf.resolve_char_ref() {
                            p.push(c);
                        } else {
                            let name = String::from_utf8_lossy(&rf);
                            p.push_str(match name.as_ref() { "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", _ => "" });
                        }
                    }
                }
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                match name.as_slice() {
                    b"t" => in_t = false,
                    b"p" if para.is_some() => {
                        let text = para.take().unwrap_or_default();
                        match cell.as_mut() {
                            Some(c) => c.push(text),
                            None => paras.push(text),
                        }
                    }
                    b"tc" if cell.is_some() => row.push(cell.take().unwrap_or_default().join("\n")),
                    b"tr" if shape_depth.is_some() => table_rows.push(row.join(" | ")),
                    _ => {}
                }
                if Some(stack.len()) == shape_depth {
                    shape_depth = None;
                    if !only_body_placeholder || is_body {
                        if !table_rows.is_empty() {
                            out.append(&mut table_rows);
                        } else if !paras.is_empty() {
                            out.push(paras.join("\n"));
                        }
                    }
                    paras.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// PowerPoint: slide by slide ("Slide n:"), their shapes' text and tables, and the notes.
fn read_pptx(path: &Path) -> ReadResult<String> {
    let mut z = open_zip(path)?;
    let pres = part(&mut z, "ppt/presentation.xml")?.ok_or(ReadError::Damaged("document"))?;
    let rel = rels(&mut z, "ppt/presentation.xml")?;
    let mut ids = Vec::new();
    let mut r = Reader::from_reader(pres.as_slice());
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"sldId" => {
                if let Some(rid) = attr_rid(&e) {
                    ids.push(rid);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let mut parts = Vec::new();
    for (n, rid) in ids.iter().enumerate() {
        let Some((_, target, _)) = rel.iter().find(|(id, _, _)| id == rid) else { continue };
        parts.push(format!("\n\nSlide {}:", n + 1));
        if let Some(xml) = part(&mut z, target)? {
            parts.extend(shapes_text(&xml, false));
        }
        let notes = rels(&mut z, target)?.into_iter().find(|(_, _, kind)| kind.ends_with("/notesSlide"));
        if let Some((_, notes_path, _)) = notes {
            if let Some(xml) = part(&mut z, &notes_path)? {
                let text = shapes_text(&xml, true).join("\n");
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(format!("Notes: {text}"));
                }
            }
        }
    }
    Ok(parts.join("\n"))
}

fn attr_rid(e: &quick_xml::events::BytesStart) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == b"r:id" || a.key.as_ref().ends_with(b":id") && a.key.as_ref() != b"id").map(|a| {
        String::from_utf8_lossy(&a.value).into_owned()
    })
}

fn cell_value(v: &calamine::Data, old_format: bool) -> Option<String> {
    use calamine::Data;
    match v {
        Data::Empty => None,
        Data::Error(_) if old_format => None,
        Data::Error(e) => Some(format!("{e}")),
        Data::String(s) if old_format => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Data::String(s) => Some(s.clone()).filter(|s| !s.is_empty()),
        Data::Bool(b) if old_format => Some(if *b { "TRUE" } else { "FALSE" }.into()),
        Data::Bool(b) => Some(if *b { "True" } else { "False" }.into()),
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) => Some(if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", *f as i64) } else { format!("{f}") }),
        Data::DateTime(d) if !old_format && d.as_f64() < 1.0 && d.as_f64() >= 0.0 => {
            // a time of day alone
            let secs = (d.as_f64() * 86400.0).round() as i64;
            Some(format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60))
        }
        Data::DateTime(d) => match d.as_datetime() {
            Some(dt) => {
                let s = dt.format("%Y-%m-%d %H:%M:%S").to_string();
                Some(if old_format { s.trim_end_matches(" 00:00:00").to_string() } else { s })
            }
            None => Some(d.as_f64().to_string()),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => Some(s.clone()),
    }
}

/// Excel (any age) and OpenDocument spreadsheets: sheet by sheet, rows as "a | b".
fn read_spreadsheet(path: &Path, old_format: bool) -> ReadResult<String> {
    use calamine::Reader as _;
    let opened = if old_format && biff8(path) {
        // Excel 97 and later store their strings as Unicode, whatever code page the file names.
        let mut options = calamine::XlsOptions::default();
        options.force_codepage = Some(1200);
        std::fs::File::open(path)
            .map_err(calamine::Error::Io)
            .and_then(|f| {
                calamine::Xls::new_with_options(std::io::BufReader::new(f), options)
                    .map(calamine::Sheets::Xls)
                    .map_err(calamine::Error::Xls)
            })
    } else {
        calamine::open_workbook_auto(path)
    };
    let mut book = match opened {
        Ok(b) => b,
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("encrypt") || msg.contains("password") {
                return Err(ReadError::Skip("password"));
            }
            return Err(ReadError::Damaged("document"));
        }
    };
    let mut parts = Vec::new();
    let mut rows = 0usize;
    for name in book.sheet_names().to_vec() {
        parts.push(format!("\n\nSheet {name}:"));
        let Ok(range) = book.worksheet_range(&name) else { continue };
        for row in range.rows() {
            let cells: Vec<String> = row.iter().filter_map(|c| cell_value(c, old_format)).collect();
            if !cells.is_empty() {
                parts.push(cells.join(" | "));
                rows += 1;
            }
            if rows > MAX_ROWS {
                return Ok(parts.join("\n"));
            }
        }
    }
    Ok(parts.join("\n"))
}

/// The preview picture Office saves into some files ("Save thumbnail").
#[allow(dead_code)]
pub fn ooxml_thumbnail(path: &Path) -> Option<Vec<u8>> {
    zip_picture(path, &["docProps/thumbnail.jpeg", "docProps/thumbnail.jpg", "docProps/thumbnail.png"])
}

// ---- Office 97-2003 (and password-protected modern files): OLE compound files ----------

/// Whether an old Excel file is Excel 97 or later (BIFF8).
fn biff8(path: &Path) -> bool {
    let Ok(mut ole) = cfb::open(path) else { return false };
    let name = if ole.exists("/Workbook") { "/Workbook" } else { "/Book" };
    let Ok(mut s) = ole.open_stream(name) else { return false };
    let mut head = [0u8; 6];
    if s.read_exact(&mut head).is_err() {
        return false;
    }
    u16::from_le_bytes([head[0], head[1]]) == 0x0809 && u16::from_le_bytes([head[4], head[5]]) == 0x0600
}

fn read_ole(path: &Path) -> ReadResult<String> {
    let mut ole = cfb::open(path).map_err(|_| ReadError::Damaged("document"))?;
    if ole.exists("/EncryptedPackage") || ole.exists("/EncryptionInfo") {
        return Err(ReadError::Skip("password")); // a modern Office file with a password
    }
    if ole.exists("/WordDocument") {
        return doc_text(&mut ole);
    }
    if ole.exists("/PowerPoint Document") {
        return ppt_text(&mut ole);
    }
    if ole.exists("/Workbook") || ole.exists("/Book") {
        drop(ole);
        return read_spreadsheet(path, true);
    }
    Err(ReadError::Damaged("document"))
}

fn stream(ole: &mut cfb::CompoundFile<std::fs::File>, name: &str) -> ReadResult<Vec<u8>> {
    let mut s = ole.open_stream(name).map_err(|_| ReadError::Damaged("document"))?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).map_err(|_| ReadError::Damaged("document"))?;
    Ok(out)
}

fn u16_at(b: &[u8], at: usize) -> ReadResult<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or(ReadError::Damaged("document"))
}

fn u32_at(b: &[u8], at: usize) -> ReadResult<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or(ReadError::Damaged("document"))
}

fn cp1252(b: &[u8]) -> String {
    encoding_rs::WINDOWS_1252.decode_without_bom_handling(b).0.into_owned()
}

fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Word 97-2003 ([MS-DOC]): the main text, put together from the pieces the piece table lists
/// (fast-saved files keep their text out of order).
fn doc_text(ole: &mut cfb::CompoundFile<std::fs::File>) -> ReadResult<String> {
    let wd = stream(ole, "/WordDocument")?;
    if wd.len() < 0x40 || u16_at(&wd, 0)? != 0xA5EC {
        return Err(ReadError::Damaged("document"));
    }
    let (nfib, flags) = (u16_at(&wd, 2)?, u16_at(&wd, 0x0A)?);
    if flags & 0x0100 != 0 || flags & 0x8000 != 0 {
        return Err(ReadError::Skip("password")); // fEncrypted, fObfuscated
    }
    if nfib < 101 {
        // Word 6 and 95: one run of single-byte text
        let (fc_min, fc_mac) = (u32_at(&wd, 0x18)? as usize, u32_at(&wd, 0x1C)? as usize);
        let end = fc_mac.min(fc_min + MAX_CHARS).min(wd.len());
        return Ok(doc_clean(&cp1252(wd.get(fc_min..end).unwrap_or_default())));
    }
    let table_name = if flags & 0x0200 != 0 { "/1Table" } else { "/0Table" };
    if !ole.exists(table_name) {
        return Err(ReadError::Damaged("document"));
    }
    let table = stream(ole, table_name)?;
    let csw = u16_at(&wd, 0x20)? as usize;
    let lw = 0x22 + csw * 2 + 2; // FibRgLw97
    let ccp_text = u32_at(&wd, lw + 12)? as usize;
    let rg = lw + u16_at(&wd, lw - 2)? as usize * 4 + 2; // FibRgFcLcb
    if u16_at(&wd, rg - 2)? <= 33 {
        return Err(ReadError::Damaged("document"));
    }
    let fc_clx = u32_at(&wd, rg + 33 * 8)? as usize;
    let lcb_clx = u32_at(&wd, rg + 33 * 8 + 4)? as usize;
    let clx = table.get(fc_clx..fc_clx + lcb_clx).ok_or(ReadError::Damaged("document"))?;
    Ok(doc_clean(&pieces(&wd, clx, ccp_text)?))
}

fn pieces(wd: &[u8], clx: &[u8], limit: usize) -> ReadResult<String> {
    let mut i = 0;
    while i < clx.len() && clx[i] == 0x01 {
        i += 3 + u16_at(clx, i + 1)? as usize; // Prc: formatting, not text
    }
    if i + 5 > clx.len() || clx[i] != 0x02 {
        return Err(ReadError::Damaged("document"));
    }
    let lcb = u32_at(clx, i + 1)? as usize;
    let plc = clx.get(i + 5..i + 5 + lcb).ok_or(ReadError::Damaged("document"))?;
    if lcb < 4 {
        return Ok(String::new());
    }
    let n = (lcb - 4) / 12;
    let mut cps = Vec::with_capacity(n + 1);
    for k in 0..=n {
        cps.push(u32_at(plc, k * 4)? as usize);
    }
    let mut out = String::new();
    let mut total = 0;
    for k in 0..n {
        let (start, end) = (cps[k], cps[k + 1].min(limit));
        if start >= limit || total > MAX_CHARS {
            break;
        }
        if end <= start {
            continue;
        }
        let fc = u32_at(plc, (n + 1) * 4 + k * 8 + 2)? as usize;
        if fc & 0x4000_0000 != 0 {
            // fCompressed: one byte per character
            let at = (fc & 0x3FFF_FFFF) / 2;
            out.push_str(&cp1252(wd.get(at..(at + end - start).min(wd.len())).unwrap_or_default()));
        } else {
            out.push_str(&utf16le(wd.get(fc..(fc + 2 * (end - start)).min(wd.len())).unwrap_or_default()));
        }
        total += end - start;
    }
    Ok(out)
}

/// Word's special characters: paragraph and cell marks into line breaks and bars, objects out,
/// and of each field only its result (not its instruction).
fn doc_clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fields: Vec<bool> = Vec::new(); // per open field: still in its instruction?
    for ch in text.chars() {
        match ch {
            '\u{13}' => fields.push(true),
            '\u{14}' => {
                if let Some(f) = fields.last_mut() {
                    *f = false;
                }
            }
            '\u{15}' => {
                fields.pop();
            }
            _ if fields.contains(&true) => {}
            '\r' | '\u{b}' | '\u{c}' => out.push('\n'),
            '\u{7}' => out.push_str(" | "),
            '\u{1e}' => out.push('-'),
            '\u{a0}' => out.push(' '),
            '\u{1}' | '\u{2}' | '\u{3}' | '\u{4}' | '\u{5}' | '\u{8}' | '\u{1f}' => {}
            c => out.push(c),
        }
    }
    out
}

/// PowerPoint 97-2003 ([MS-PPT]): records, some of which hold text.
fn ppt_text(ole: &mut cfb::CompoundFile<std::fs::File>) -> ReadResult<String> {
    if ole.exists("/EncryptedSummary") {
        return Err(ReadError::Skip("password"));
    }
    let data = stream(ole, "/PowerPoint Document")?;
    let mut parts: Vec<String> = Vec::new();
    let (mut slides, mut chars) = (0usize, 0usize);
    ppt_walk(&data, 0, data.len(), 0, false, &mut parts, &mut slides, &mut chars)?;
    Ok(parts.join("\n"))
}

#[allow(clippy::too_many_arguments)]
fn ppt_walk(
    data: &[u8], start: usize, end: usize, depth: usize, in_slide_list: bool, parts: &mut Vec<String>,
    slides: &mut usize, chars: &mut usize,
) -> ReadResult<()> {
    const TEXT_CHARS: u16 = 0x0FA0;
    const TEXT_BYTES: u16 = 0x0FA8;
    const SLIDE_LIST: u16 = 0x0FF0;
    const SLIDE_PERSIST: u16 = 0x03F3;
    const SKIP: [u16; 3] = [0x03F8, 0x0FC9, 0x0FD9]; // masters, handouts, the outline view's placeholders
    const CRYPT: u16 = 0x2F14;
    let mut pos = start;
    while pos + 8 <= end && *chars <= MAX_CHARS {
        let ver_inst = u16_at(data, pos)?;
        let rtype = u16_at(data, pos + 2)?;
        let rlen = u32_at(data, pos + 4)? as usize;
        let (body, next) = (pos + 8, pos + 8 + rlen);
        if next > end {
            return Ok(());
        }
        if rtype == CRYPT {
            return Err(ReadError::Skip("password"));
        }
        if ver_inst & 0xF == 0xF {
            // a container
            let instance = ver_inst >> 4;
            if rtype == SLIDE_LIST && instance == 1 {
                // the masters' list
            } else if !SKIP.contains(&rtype) && depth < 32 {
                ppt_walk(data, body, next, depth + 1, rtype == SLIDE_LIST && instance == 0, parts, slides, chars)?;
            }
        } else if rtype == SLIDE_PERSIST && in_slide_list {
            *slides += 1;
            parts.push(format!("\n\nSlide {}:", slides));
        } else if rtype == TEXT_CHARS || rtype == TEXT_BYTES {
            let raw = &data[body..next];
            let text = if rtype == TEXT_CHARS { utf16le(raw) } else { raw.iter().map(|&b| b as char).collect() };
            let text = text.replace('\r', "\n").replace('\u{b}', "\n");
            let text = text.trim().to_string();
            if !text.is_empty() && parts.last() != Some(&text) {
                *chars += text.chars().count();
                parts.push(text);
            }
        }
        pos = next;
    }
    Ok(())
}

// ---- OpenDocument (LibreOffice, OpenOffice) --------------------------------------------

const TEXT_NS: &[u8] = b"urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const TABLE_NS: &[u8] = b"urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const DRAW_NS: &[u8] = b"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";
const OFFICE_NS: &[u8] = b"urn:oasis:names:tc:opendocument:xmlns:office:1.0";

fn read_odf(path: &Path) -> ReadResult<String> {
    if suffix(&path.to_string_lossy()).starts_with(".fod") {
        return odf_text(&std::fs::read(path)?); // flat: one XML file
    }
    let mut z = open_zip(path)?;
    if let Some(manifest) = part(&mut z, "META-INF/manifest.xml")? {
        if manifest.windows(15).any(|w| w == b"encryption-data") {
            return Err(ReadError::Skip("password"));
        }
    }
    let content = part(&mut z, "content.xml")?.ok_or(ReadError::Damaged("document"))?;
    odf_text(&content)
}

fn odf_text(xml: &[u8]) -> ReadResult<String> {
    let mut r = NsReader::from_reader(xml);
    let mut out: Vec<String> = Vec::new();
    let (mut size, mut rows, mut pages) = (0usize, 0usize, 0usize);
    let mut mode = "text";
    let mut cells: Vec<Vec<String>> = Vec::new(); // per open table cell: its paragraphs
    let mut row: Vec<Vec<String>> = Vec::new(); // per open table row: its cells
    let mut skipping = 0usize; // inside tracked changes
    let mut paras: Vec<String> = Vec::new(); // open paragraphs' text (they can nest, e.g. footnotes)
    let mut buf = Vec::new();
    let push_text = |paras: &mut Vec<String>, s: &str| {
        if let Some(p) = paras.last_mut() {
            p.push_str(s);
        }
    };
    loop {
        let (ns, ev) = match r.read_resolved_event_into(&mut buf) {
            Ok(x) => x,
            Err(_) => {
                if out.is_empty() {
                    return Err(ReadError::Damaged("document"));
                }
                break;
            }
        };
        let ns: Vec<u8> = match ns {
            ResolveResult::Bound(n) => n.as_ref().to_vec(),
            _ => vec![],
        };
        match ev {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_vec();
                match (ns.as_slice(), name.as_slice()) {
                    (TEXT_NS, b"tracked-changes") => skipping += 1,
                    (TEXT_NS, b"p") | (TEXT_NS, b"h") => paras.push(String::new()),
                    (TABLE_NS, b"table-cell") => cells.push(Vec::new()),
                    (TABLE_NS, b"table-row") => row.push(Vec::new()),
                    (OFFICE_NS, b"spreadsheet") => mode = "sheet",
                    (OFFICE_NS, b"presentation") => mode = "slides",
                    (TABLE_NS, b"table") if mode == "sheet" && cells.is_empty() => {
                        out.push(format!("\n\nSheet {}:", attr(&e, b"name").unwrap_or_default()));
                    }
                    (DRAW_NS, b"page") => {
                        pages += 1;
                        out.push(format!("\n\n{} {}:", if mode == "slides" { "Slide" } else { "Page" }, pages));
                    }
                    (TEXT_NS, b"s") => push_text(&mut paras, &" ".repeat(space_count(&e))),
                    (TEXT_NS, b"tab") => push_text(&mut paras, "\t"),
                    (TEXT_NS, b"line-break") => push_text(&mut paras, "\n"),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.local_name().as_ref().to_vec();
                match (ns.as_slice(), name.as_slice()) {
                    (TEXT_NS, b"s") => push_text(&mut paras, &" ".repeat(space_count(&e))),
                    (TEXT_NS, b"tab") => push_text(&mut paras, "\t"),
                    (TEXT_NS, b"line-break") => push_text(&mut paras, "\n"),
                    (TEXT_NS, b"p") | (TEXT_NS, b"h") => {}
                    (TABLE_NS, b"table-cell") => {}
                    (DRAW_NS, b"page") => {
                        pages += 1;
                        out.push(format!("\n\n{} {}:", if mode == "slides" { "Slide" } else { "Page" }, pages));
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if skipping == 0 {
                    push_text(&mut paras, &xml_text(&t));
                }
            }
            Event::GeneralRef(rf) => {
                if skipping == 0 {
                    let s = if let Ok(Some(c)) = rf.resolve_char_ref() {
                        c.to_string()
                    } else {
                        match String::from_utf8_lossy(&rf).as_ref() {
                            "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", _ => "",
                        }
                        .to_string()
                    };
                    push_text(&mut paras, &s);
                }
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_vec();
                match (ns.as_slice(), name.as_slice()) {
                    (TEXT_NS, b"tracked-changes") => skipping = skipping.saturating_sub(1),
                    (TEXT_NS, b"p") | (TEXT_NS, b"h") => {
                        let text = paras.pop().unwrap_or_default();
                        if skipping == 0 {
                            let text = text.trim();
                            if !text.is_empty() {
                                size += text.chars().count();
                                match cells.last_mut() {
                                    Some(c) => c.push(text.to_string()),
                                    None => out.push(text.to_string()),
                                }
                            }
                        }
                    }
                    (TABLE_NS, b"table-cell") if !cells.is_empty() => {
                        let text = cells.pop().unwrap_or_default().join(" ");
                        if let (Some(r), false) = (row.last_mut(), text.is_empty()) {
                            r.push(text);
                        } else if !text.is_empty() {
                            out.push(text);
                        }
                    }
                    (TABLE_NS, b"table-row") if !row.is_empty() => {
                        let values = row.pop().unwrap_or_default();
                        if !values.is_empty() {
                            let line = values.join(" | ");
                            match cells.last_mut() {
                                Some(c) => c.push(line),
                                None => out.push(line),
                            }
                            rows += 1;
                        }
                    }
                    _ => {}
                }
                if size > MAX_CHARS || rows > MAX_ROWS {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out.join("\n"))
}

fn space_count(e: &quick_xml::events::BytesStart) -> usize {
    attr(e, b"c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1).min(100)
}

/// The preview picture LibreOffice saves into every document.
pub fn odf_thumbnail(path: &Path) -> Option<Vec<u8>> {
    zip_picture(path, &["Thumbnails/thumbnail.png"])
}

fn zip_picture(path: &Path, wanted: &[&str]) -> Option<Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    for name in wanted {
        if let Ok(f) = z.by_name(name) {
            if f.size() <= 4 * 1024 * 1024 {
                let mut out = Vec::new();
                f.take(4 * 1024 * 1024).read_to_end(&mut out).ok()?;
                return Some(out);
            }
        }
    }
    None
}

// ---- RTF ------------------------------------------------------------------------------

/// Groups whose text isn't part of the document: fonts, styles, pictures, field codes, ...
static RTF_SKIP: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    "aftncn aftnsep aftnsepc annotation atnauthor atndate atnicn atnid atnparent atnref atntime atrfend atrfstart \
     author background bkmkend bkmkstart blipuid buptim category colorschememapping colortbl comment company creatim \
     datafield datastore defchp defpap do doccomm docvar dptxbxtext ebcend ebcstart factoidname falt fchars ffdeftext \
     ffentrymcr ffexitmcr ffformat ffhelptext ffl ffname ffstattext file filetbl fldinst fldtype fname fontemb \
     fontfile fonttbl footer footerf footerl footerr formfield ftncn ftnsep ftnsepc g generator gridtbl header headerf \
     headerl headerr hl hlfr hlinkbase hlloc hlsrc hsv htmltag info keycode keywords latentstyles lchars levelnumbers \
     leveltext lfolevel linkval list listlevel listname listoverride listoverridetable listpicture liststylename \
     listtable listtext lsdlockedexcept macc maccPr mailmerge maln malnScr manager margPr mbar mbarPr mbaseJc mbegChr \
     mborderBox mborderBoxPr mbox mboxPr mchr mcount mctrlPr md mdeg mdegHide mden mdiff mdPr me mendChr meqArr \
     meqArrPr mf mfName mfPr mfunc mfuncPr mgroupChr mgroupChrPr mgrow mhideBot mhideLeft mhideRight mhideTop \
     mhtmltag mlim mlimloc mlimlow mlimlowPr mlimupp mlimuppPr mm mmaddfieldname mmath mmathPict mmathPr mmaxdist mmc \
     mmcJc mmconnectstr mmconnectstrdata mmcPr mmcs mmdatasource mmheadersource mmmailsubject mmodso mmodsofilter \
     mmodsofldmpdata mmodsomappedname mmodsoname mmodsorecipdata mmodsosort mmodsosrc mmodsotable mmodsoudl \
     mmodsoudldata mmodsouniquetag mmPr mmquery mmr mnary mnaryPr mnoBreak mnum mobjDist moMath moMathPara \
     moMathParaPr mopEmu mphant mphantPr mplcHide mpos mr mrad mradPr mrPr msepChr mshow mshp msPre msPrePr msSub \
     msSubPr msSubSup msSubSupPr msSup msSupPr mstrikeBLTR mstrikeH mstrikeTLBR mstrikeV msub msubHide msup msupHide \
     mtransp mtype mvertJc mvfmf mvfml mvtof mvtol mzeroAsc mzeroDesc mzeroWid nesttableprops nextfile nonesttables \
     nonshppict objalias objclass objdata object objname objsect objtime oldcprops oldpprops oldsprops oldtprops \
     oleclsid operator panose password passwordhash pgp pgptbl picprop pict pn pnseclvl pntext pntxta pntxtb printim \
     private propname protend protstart protusertbl pxe result revtbl revtim rsidtbl rxe shp shpgrp shpinst shppict \
     shprslt shptxt sn sp staticval stylesheet subject sv svb tc template themedata title txe ud upr userprops \
     wgrffmtfilter windowcaption writereservation writereservhash xe xform xmlattrname xmlattrvalue xmlclose xmlname \
     xmlnstbl xmlopen"
        .split_whitespace()
        .collect()
});

fn rtf_char(word: &str) -> Option<&'static str> {
    Some(match word {
        "par" => "\n", "sect" | "page" => "\n\n", "line" => "\n", "tab" => "\t", "cell" | "nestcell" => " | ",
        "row" | "nestrow" => "\n", "emdash" => "\u{2014}", "endash" => "\u{2013}",
        "emspace" | "enspace" | "qmspace" => " ", "bullet" => "\u{2022}", "lquote" => "\u{2018}",
        "rquote" => "\u{2019}", "ldblquote" => "\u{201c}", "rdblquote" => "\u{201d}",
        _ => return None,
    })
}

static RTF_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\([a-zA-Z]{1,32})(-?\d{1,10})? ?|\\'([0-9a-fA-F]{2})|\\([^a-zA-Z])|([{}])|[\r\n]+|([^\\{}\r\n]+)").unwrap()
});

fn read_rtf(path: &Path) -> ReadResult<String> {
    let data = std::fs::read(path)?;
    Ok(rtf_text(&data.iter().map(|&b| b as char).collect::<String>()))
}

fn codepage(cp: i64) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match cp {
        874 => WINDOWS_874, 932 => SHIFT_JIS, 936 => GBK, 949 => EUC_KR, 950 => BIG5, 1250 => WINDOWS_1250,
        1251 => WINDOWS_1251, 1252 => WINDOWS_1252, 1253 => WINDOWS_1253, 1254 => WINDOWS_1254, 1255 => WINDOWS_1255,
        1256 => WINDOWS_1256, 1257 => WINDOWS_1257, 1258 => WINDOWS_1258, 866 => IBM866, 10000 => MACINTOSH,
        _ => return None,
    })
}

/// RTF's text without its markup. (The file is read one character per byte; RTF itself is ASCII,
/// with other characters escaped in the document's code page.)
pub fn rtf_text(rtf: &str) -> String {
    let mut out = String::new();
    let mut pending: Vec<u8> = Vec::new(); // \'hh bytes, decoded together (a character can take two)
    let mut stack: Vec<(i64, bool)> = Vec::new();
    let (mut ignorable, mut uc, mut skip) = (false, 1i64, 0i64);
    let mut enc = encoding_rs::WINDOWS_1252;
    let mut high: Option<u16> = None; // a \u high surrogate waiting for its pair
    let bytes = rtf.as_bytes();
    let mut pos = 0usize;
    let flush = |pending: &mut Vec<u8>, out: &mut String, enc: &'static encoding_rs::Encoding| {
        if !pending.is_empty() {
            out.push_str(&enc.decode_without_bom_handling(pending).0);
            pending.clear();
        }
    };
    while pos < bytes.len() {
        let Some(m) = RTF_TOKEN.captures_at(rtf, pos) else { break };
        let whole = m.get(0).unwrap();
        if whole.start() != pos {
            pos += rtf[pos..].chars().next().map(char::len_utf8).unwrap_or(1);
            continue;
        }
        pos = whole.end();
        if let Some(hex) = m.get(3) {
            if skip > 0 {
                skip -= 1;
            } else if !ignorable {
                pending.push(u8::from_str_radix(hex.as_str(), 16).unwrap_or(b'?'));
            }
            continue;
        }
        flush(&mut pending, &mut out, enc);
        if let Some(brace) = m.get(5) {
            skip = 0;
            if brace.as_str() == "{" {
                stack.push((uc, ignorable));
            } else if let Some((u, ig)) = stack.pop() {
                uc = u;
                ignorable = ig;
            }
        } else if let Some(sym) = m.get(4) {
            skip = 0;
            let s = sym.as_str();
            if s == "*" {
                ignorable = true;
            } else if !ignorable {
                match s {
                    "~" => out.push('\u{a0}'),
                    "{" | "}" | "\\" => out.push_str(s),
                    "\r" | "\n" => out.push('\n'), // a backslash before a line break is a paragraph mark
                    "_" => out.push('-'),
                    _ => {}
                }
            }
        } else if let Some(word) = m.get(1) {
            skip = 0;
            let word = word.as_str();
            let arg = m.get(2).and_then(|a| a.as_str().parse::<i64>().ok());
            if RTF_SKIP.contains(word) {
                ignorable = true;
            } else if word == "bin" && arg.is_some() {
                pos += arg.unwrap_or(0).max(0) as usize; // raw binary data follows
                while pos < bytes.len() && !rtf.is_char_boundary(pos) {
                    pos += 1;
                }
            } else if word == "ansicpg" && arg.is_some() {
                if let Some(e) = codepage(arg.unwrap_or(1252)) {
                    enc = e;
                }
            } else if ignorable {
            } else if word == "uc" && arg.is_some() {
                uc = arg.unwrap_or(1).max(0);
            } else if word == "u" && arg.is_some() {
                let code = arg.unwrap_or(0);
                let unit = (if code < 0 { code + 0x10000 } else { code }) as u32;
                if (0xD800..0xDC00).contains(&unit) {
                    high = Some(unit as u16);
                } else if (0xDC00..0xE000).contains(&unit) {
                    if let Some(h) = high.take() {
                        out.push_str(&String::from_utf16_lossy(&[h, unit as u16]));
                    }
                } else if let Some(c) = char::from_u32(unit) {
                    out.push(c);
                }
                skip = uc;
            } else if let Some(c) = rtf_char(word) {
                out.push_str(c);
            }
        } else if let Some(plain) = m.get(6) {
            let mut text = plain.as_str();
            if skip > 0 {
                let taken = (skip as usize).min(text.chars().count());
                skip -= taken as i64;
                let at = text.char_indices().nth(taken).map(|(i, _)| i).unwrap_or(text.len());
                text = &text[at..];
            }
            if !text.is_empty() && !ignorable {
                out.push_str(text);
            }
        }
        if out.len() > 4 * MAX_CHARS {
            break;
        }
    }
    flush(&mut pending, &mut out, enc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf_markup_goes() {
        let text = rtf_text(r"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}\f0 Hello \b World\b0\par Gr\'fc\'dfe \u8364?}");
        assert_eq!(text, "Hello World\nGrüße €");
    }

    #[test]
    fn word_marks_become_lines_and_bars() {
        assert_eq!(doc_clean("a\rb\u{7}c\u{13}HYPERLINK x\u{14}link\u{15}"), "a\nb | clink");
    }
}
