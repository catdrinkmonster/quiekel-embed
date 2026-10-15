//! Emails saved as files: .eml (the standard), .msg (Outlook) and saved web pages (.mht, built
//! like an email). An email's text is its subject, who wrote it to whom and when, and its body.
//! Its attachments are searched like files inside an archive (see archive.rs); pictures shown
//! inside the text (logos, signatures) are left out.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::sync::{Arc, LazyLock};

use anyhow::{Result, anyhow, bail};
use parking_lot::Mutex;
use regex::Regex;

use crate::extract::{MAX_CHARS, OLE_MAGIC, strip_html};

static CID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)cid:([^"'\s>)]+)"#).unwrap());

type Ole = Arc<Mutex<cfb::CompoundFile<Cursor<Vec<u8>>>>>;

enum Payload {
    Bytes(Vec<u8>),
    Ole { file: Ole, stream: String },
}

pub struct Attachment {
    pub name: String,
    pub size: u64,
    pub inline: bool, // a picture shown inside the text
    payload: Payload,
}

impl Attachment {
    pub fn load(&self) -> Result<Vec<u8>> {
        match &self.payload {
            Payload::Bytes(b) => Ok(b.clone()),
            Payload::Ole { file, stream } => {
                let mut f = file.lock();
                let mut s = f.open_stream(stream)?;
                let mut out = Vec::new();
                s.read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }
}

#[derive(Default)]
pub struct Mail {
    pub subject: String,
    pub sender: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    pub body: String,
    pub attachments: Vec<Attachment>,
}

impl Mail {
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = [
            ("Subject", &self.subject), ("From", &self.sender), ("To", &self.to), ("Cc", &self.cc), ("Date", &self.date),
        ]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(l, v)| format!("{l}: {v}"))
        .collect();
        let names: Vec<&str> = self.attachments.iter().filter(|a| !a.inline).map(|a| a.name.as_str()).collect();
        if !names.is_empty() {
            lines.push(format!("Attachments: {}", names.join(", ")));
        }
        let body: String = self.body.trim().chars().take(MAX_CHARS).collect();
        format!("{}\n\n{}", lines.join("\n"), body)
    }
}

/// An email with its attachments ready to be read.
pub fn open_mail(data: &[u8]) -> Result<Mail> {
    if data.starts_with(OLE_MAGIC) { msg(data.to_vec()) } else { eml(data) }
}

fn local_time(ts: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%Y-%m-%d %H:%M").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

/// photo.jpg, photo (2).jpg, ...: attachments may share a name.
fn unique(names: &mut HashSet<String>, name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    };
    let mut candidate = name.to_string();
    let mut n = 1;
    while names.contains(&candidate.to_lowercase()) {
        n += 1;
        candidate = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
    }
    names.insert(candidate.to_lowercase());
    candidate
}

fn clean_name(name: &str) -> String {
    let name = name.replace('\0', "");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = name.replace(['/', '\\'], "_");
    let name = name.trim_matches(|c| c == '.' || c == ' ');
    if name.is_empty() { "attachment".into() } else { name.to_string() }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- .eml and .mht -------------------------------------------------------------------

fn addresses(a: Option<&mail_parser::Address>) -> String {
    let Some(a) = a else { return String::new() };
    let list: Vec<String> = a
        .iter()
        .map(|addr| match (addr.name(), addr.address()) {
            (Some(n), Some(e)) => format!("{n} <{e}>"),
            (Some(n), None) => n.to_string(),
            (None, Some(e)) => e.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    collapse(&list.join(", "))
}

fn extension_for(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => ".jpg", "image/png" => ".png", "image/gif" => ".gif", "application/pdf" => ".pdf",
        "text/plain" => ".txt", "text/html" => ".html", "application/zip" => ".zip", "message/rfc822" => ".eml",
        "image/webp" => ".webp", "text/calendar" => ".ics", "text/csv" => ".csv",
        _ => ".bin",
    }
}

/// The parts that hold content, in order; an attached email counts as one part.
fn collect_leaves(msg: &mail_parser::Message, idx: u32, top: bool, out: &mut Vec<u32>) {
    use mail_parser::PartType;
    let Some(part) = msg.parts.get(idx as usize) else { return };
    match &part.body {
        PartType::Message(_) if !top => out.push(idx),
        PartType::Multipart(children) => {
            for &c in children {
                collect_leaves(msg, c, false, out);
            }
        }
        _ => out.push(idx),
    }
}

fn eml(data: &[u8]) -> Result<Mail> {
    use mail_parser::{MimeHeaders, PartType};
    let msg = mail_parser::MessageParser::default().parse(data).ok_or_else(|| anyhow!("not an email"))?;
    let mut mail = Mail {
        subject: collapse(msg.subject().unwrap_or("")),
        sender: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        date: msg.date().map(|d| local_time(d.to_timestamp())).unwrap_or_default(),
        ..Default::default()
    };
    // The body: plain text first, else the HTML one without its tags.
    let plain = msg.text_body.first().copied();
    let html = msg.html_body.first().copied();
    let plain_is_html = plain.map(|i| msg.parts.get(i as usize).map(|p| matches!(p.body, PartType::Html(_))).unwrap_or(false)).unwrap_or(false);
    mail.body = match plain.and_then(|i| msg.parts.get(i as usize)) {
        Some(p) => match &p.body {
            PartType::Text(t) => t.to_string(),
            PartType::Html(h) => strip_html(h),
            _ => String::new(),
        },
        None => String::new(),
    };
    let html_text = html
        .and_then(|i| msg.parts.get(i as usize))
        .and_then(|p| match &p.body {
            PartType::Html(h) => Some(h.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let cids: HashSet<String> = CID.captures_iter(&html_text).map(|c| c[1].to_string()).collect();
    let _ = plain_is_html;
    let html = html.filter(|&i| matches!(msg.parts.get(i as usize).map(|p| &p.body), Some(PartType::Html(_))));
    let mut leaves = Vec::new();
    collect_leaves(&msg, 0, true, &mut leaves);
    let mut names = HashSet::new();
    for (i, &idx) in leaves.iter().enumerate() {
        if Some(idx) == plain || Some(idx) == html {
            continue;
        }
        let Some(part) = msg.parts.get(idx as usize) else { continue };
        let i = i as u32;
        let filename = part.attachment_name().unwrap_or("").to_string();
        let (payload, mut name) = match &part.body {
            PartType::Message(inner) => {
                let title = if !filename.is_empty() {
                    filename
                } else {
                    Some(collapse(inner.subject().unwrap_or(""))).filter(|s| !s.is_empty()).unwrap_or_else(|| "email".into())
                };
                let name = if title.to_lowercase().ends_with(".eml") { title } else { format!("{title}.eml") };
                (inner.raw_message().to_vec(), name)
            }
            _ => {
                let name = filename;
                let disposition_attachment = part.content_disposition().map(|d| d.is_attachment()).unwrap_or(false);
                if name.is_empty() && !disposition_attachment {
                    continue; // another version of the text (HTML next to plain text), not an attachment
                }
                let ctype = part
                    .content_type()
                    .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("")).to_lowercase())
                    .unwrap_or_default();
                let name = if name.is_empty() { format!("attachment{}{}", i + 1, extension_for(&ctype)) } else { name };
                (part.contents().to_vec(), name)
            }
        };
        let cid = part.content_id().unwrap_or("").trim().trim_matches(['<', '>']).to_string();
        let attachment = part.content_disposition().map(|d| d.is_attachment()).unwrap_or(false);
        let inline = !cid.is_empty() && cids.contains(&cid) && !attachment;
        name = unique(&mut names, &clean_name(&name));
        mail.attachments.push(Attachment { name, size: payload.len() as u64, inline, payload: Payload::Bytes(payload) });
    }
    Ok(mail)
}

// ---- .msg (Outlook) ------------------------------------------------------------------
// An OLE compound file ([MS-OXMSG]): properties as streams named after their ID and type.

const UNICODE: u16 = 0x001F;
const STRING8: u16 = 0x001E;
const BINARY: u16 = 0x0102;

struct Props {
    ole: Ole,
    prefix: String,
    fixed: std::collections::HashMap<u16, (u16, [u8; 8])>,
    codepage: &'static encoding_rs::Encoding,
}

fn read_stream(ole: &Ole, name: &str) -> Option<Vec<u8>> {
    let mut f = ole.lock();
    if !f.exists(name) {
        return None;
    }
    let mut s = f.open_stream(name).ok()?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).ok()?;
    Some(out)
}

fn codec(cp: Option<i32>) -> &'static encoding_rs::Encoding {
    use encoding_rs::*;
    match cp {
        None | Some(0) => WINDOWS_1252,
        Some(65001) => UTF_8,
        Some(20127) | Some(28591) => WINDOWS_1252,
        Some(1200) => UTF_16LE,
        Some(c) => encoding_rs::Encoding::for_label(format!("windows-{c}").as_bytes())
            .or_else(|| encoding_rs::Encoding::for_label(format!("cp{c}").as_bytes()))
            .unwrap_or(WINDOWS_1252),
    }
}

impl Props {
    fn new(ole: &Ole, prefix: &str, header: usize) -> Props {
        let mut fixed = std::collections::HashMap::new();
        if let Some(data) = read_stream(ole, &format!("{prefix}__properties_version1.0")) {
            let mut at = header;
            while at + 16 <= data.len() {
                let tag = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
                let mut value = [0u8; 8];
                value.copy_from_slice(&data[at + 8..at + 16]);
                fixed.insert((tag >> 16) as u16, ((tag & 0xFFFF) as u16, value));
                at += 16;
            }
        }
        let mut p = Props { ole: ole.clone(), prefix: prefix.to_string(), fixed, codepage: encoding_rs::WINDOWS_1252 };
        p.codepage = codec(p.int(0x3FDE).or_else(|| p.int(0x3FFD)));
        p
    }

    fn int(&self, pid: u16) -> Option<i32> {
        let (t, v) = self.fixed.get(&pid)?;
        if *t == 0x0003 || *t == 0x000B { Some(i32::from_le_bytes([v[0], v[1], v[2], v[3]])) } else { None }
    }

    fn time(&self, pid: u16) -> Option<i64> {
        let (t, v) = self.fixed.get(&pid)?;
        if *t != 0x0040 {
            return None;
        }
        let filetime = u64::from_le_bytes(*v);
        if filetime == 0 {
            return None;
        }
        Some((filetime / 10_000_000) as i64 - 11_644_473_600)
    }

    fn raw(&self, pid: u16, ptype: u16) -> Option<Vec<u8>> {
        read_stream(&self.ole, &format!("{}__substg1.0_{pid:04X}{ptype:04X}", self.prefix))
    }

    fn str(&self, pid: u16) -> String {
        if let Some(data) = self.raw(pid, UNICODE) {
            let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
        }
        match self.raw(pid, STRING8) {
            Some(data) => self.codepage.decode_without_bom_handling(&data).0.trim_end_matches('\0').to_string(),
            None => String::new(),
        }
    }

    fn html(&self) -> String {
        match self.raw(0x1013, BINARY) {
            Some(data) => self.codepage.decode_without_bom_handling(&data).0.into_owned(),
            None => self.str(0x1013),
        }
    }

    fn body(&self) -> String {
        let text = self.str(0x1000);
        if !text.trim().is_empty() {
            return text;
        }
        let html = self.html();
        if !html.trim().is_empty() {
            return strip_html(&html);
        }
        if let Some(rtf) = self.raw(0x1009, BINARY) {
            if let Ok(raw) = decompress_rtf(&rtf) {
                return crate::documents::rtf_text(&raw.iter().map(|&b| b as char).collect::<String>());
            }
        }
        String::new()
    }
}

fn msg(data: Vec<u8>) -> Result<Mail> {
    let ole: Ole = Arc::new(Mutex::new(cfb::CompoundFile::open(Cursor::new(data))?));
    let props = Props::new(&ole, "/", 32);
    let mut mail = Mail {
        subject: props.str(0x0037),
        to: props.str(0x0E04),
        cc: props.str(0x0E03),
        date: props.time(0x0039).or_else(|| props.time(0x0E06)).map(local_time).unwrap_or_default(),
        body: props.body(),
        ..Default::default()
    };
    let name = Some(props.str(0x0C1A)).filter(|s| !s.is_empty()).unwrap_or_else(|| props.str(0x0042));
    let mut address = Some(props.str(0x5D01)).filter(|s| !s.is_empty()).unwrap_or_else(|| props.str(0x0C1F));
    if !address.contains('@') {
        address.clear(); // an Exchange address (/O=...) means nothing outside the company
    }
    mail.sender = if !name.is_empty() && !address.is_empty() && name != address {
        format!("{name} <{address}>")
    } else if !name.is_empty() {
        name
    } else {
        address
    };
    let cids: HashSet<String> = CID.captures_iter(&props.html()).map(|c| c[1].to_string()).collect();
    let mut storages: Vec<String> = {
        let f = ole.lock();
        f.read_root_storage()
            .filter(|e| e.is_storage() && e.name().to_lowercase().starts_with("__attach_version1.0_#"))
            .map(|e| e.name().to_string())
            .collect()
    };
    storages.sort();
    let mut names = HashSet::new();
    for (i, storage) in storages.iter().enumerate() {
        let att = Props::new(&ole, &format!("/{storage}/"), 8);
        let method = att.int(0x3705).unwrap_or(1);
        if method == 5 {
            // an attached email: its text becomes part of this one's
            let inner = Props::new(&ole, &format!("/{storage}/__substg1.0_3701000D/"), 24);
            mail.body.push_str(&format!("\n\n--- {} ---\n{}", inner.str(0x0037), inner.body()));
            continue;
        }
        let stream = format!("/{storage}/__substg1.0_37010102");
        let size = {
            let f = ole.lock();
            if !f.exists(&stream) {
                continue; // a link to a file elsewhere, or an embedded object
            }
            f.entry(&stream).map(|e| e.len()).unwrap_or(0)
        };
        let label = [0x3707, 0x3704, 0x3001]
            .iter()
            .map(|p| att.str(*p))
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| format!("attachment{}", i + 1));
        let cid = att.str(0x3712).trim_matches(['<', '>']).to_string();
        let inline = att.int(0x7FFE).unwrap_or(0) != 0
            || att.int(0x3714).unwrap_or(0) & 0x4 != 0
            || (!cid.is_empty() && cids.contains(&cid));
        mail.attachments.push(Attachment {
            name: unique(&mut names, &clean_name(&label)),
            size,
            inline,
            payload: Payload::Ole { file: ole.clone(), stream },
        });
    }
    Ok(mail)
}

/// Compressed RTF ([MS-OXRTFCP]): LZ77 with a dictionary that starts out with common RTF.
const RTF_DICTIONARY: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \
\\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \
\\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

pub fn decompress_rtf(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        bail!("too short");
    }
    let rd = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
    let (size, raw_size, magic) = (rd(0), rd(4), rd(8));
    if magic == 0x414C_454D {
        return Ok(data[16..(16 + raw_size).min(data.len())].to_vec()); // "MELA": stored as is
    }
    if magic != 0x7546_5A4C {
        bail!("not compressed RTF");
    }
    let mut window = vec![0u8; 4096];
    window[..RTF_DICTIONARY.len()].copy_from_slice(RTF_DICTIONARY);
    let mut write = RTF_DICTIONARY.len();
    let mut out = Vec::new();
    let (mut pos, end) = (16usize, data.len().min(size + 4));
    while pos < end && out.len() < raw_size {
        let control = data[pos];
        pos += 1;
        for bit in 0..8 {
            if pos >= end {
                break;
            }
            if control & (1 << bit) != 0 {
                if pos + 1 >= end {
                    return Ok(out);
                }
                let r = (data[pos] as usize) << 8 | data[pos + 1] as usize;
                pos += 2;
                let (offset, length) = (r >> 4, (r & 0xF) + 2);
                if offset == write {
                    return Ok(out); // the end marker
                }
                for i in 0..length {
                    let b = window[(offset + i) % 4096];
                    out.push(b);
                    window[write] = b;
                    write = (write + 1) % 4096;
                }
            } else {
                let b = data[pos];
                pos += 1;
                out.push(b);
                window[write] = b;
                write = (write + 1) % 4096;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eml_text_and_attachments() {
        let raw = b"From: Max Muster <max@example.com>\r\nTo: Erika <erika@example.com>\r\nSubject: Offer\r\n\
Date: Tue, 3 Mar 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nPrices inside.\r\n--b\r\nContent-Type: text/plain\r\n\
Content-Disposition: attachment; filename=\"prices.txt\"\r\n\r\nA 10\r\n--b--\r\n";
        let m = open_mail(raw).unwrap();
        let text = m.text();
        assert!(text.contains("Subject: Offer") && text.contains("From: Max Muster <max@example.com>"), "{text}");
        assert!(text.contains("Attachments: prices.txt") && text.contains("Prices inside."), "{text}");
        assert_eq!(m.attachments[0].load().unwrap(), b"A 10");
    }

    #[test]
    fn unique_names() {
        let mut names = HashSet::new();
        assert_eq!(unique(&mut names, "a.jpg"), "a.jpg");
        assert_eq!(unique(&mut names, "A.jpg"), "A (2).jpg");
    }
}
