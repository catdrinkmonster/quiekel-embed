//! Turn files into embeddable chunks: text passages or images.

use std::path::Path;
use std::sync::LazyLock;

use image::RgbImage;
use regex::Regex;

use crate::config::{self, suffix};

/// Why a file wasn't read.
#[derive(Debug)]
pub enum ReadError {
    /// It can't be searched by its content, and that's not an error: empty, too big, behind a
    /// password, ... (the reason's key; the page puts it in words).
    Skip(&'static str),
    /// It isn't what its name says, or it's broken: what it should have been ("document",
    /// "email", "image", "archive", "pdf").
    Damaged(&'static str),
    Io(std::io::Error),
    Other(anyhow::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Skip(k) => write!(f, "skipped: {k}"),
            ReadError::Damaged(k) => write!(f, "damaged {k}"),
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        ReadError::Io(e)
    }
}

impl From<anyhow::Error> for ReadError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast::<std::io::Error>() {
            Ok(io) => ReadError::Io(io),
            Err(e) => match e.downcast::<ReadErrorBox>() {
                Ok(b) => b.0,
                Err(e) => ReadError::Other(e),
            },
        }
    }
}

/// A ReadError carried through anyhow.
#[derive(Debug)]
pub struct ReadErrorBox(pub ReadError);
impl std::fmt::Display for ReadErrorBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ReadErrorBox {}


pub type ReadResult<T> = Result<T, ReadError>;

pub struct Chunk {
    pub snippet: String,              // shown in search results
    pub text: Option<String>,         // embedded as a document passage
    pub image: Option<RgbImage>,      // embedded as an image
}

pub struct Extracted {
    pub kind: String,
    pub chunks: Vec<Chunk>,
}

pub const MAX_CHARS: usize = config::CHUNK_CHARS * config::MAX_CHUNKS_PER_FILE;
pub const OLE_MAGIC: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1";
/// Read page by page; pages without text (scans, comics) are looked at instead.
pub const PAGED_EXTS: [&str; 2] = [".pdf", ".epub"];
const HTML_EXTS: [&str; 3] = [".html", ".htm", ".xhtml"];
/// Documents that carry a preview picture of themselves.
pub const THUMBNAIL_EXTS: [&str; 8] = [".odt", ".ott", ".ods", ".ots", ".odp", ".otp", ".odg", ".otg"];

pub fn kind_of(name: &str) -> Option<&'static str> {
    let ext = suffix(name);
    let e = ext.as_str();
    if config::IMAGE_EXTS.contains(e) {
        return Some("image");
    }
    if config::DOC_EXTS.contains(e) {
        return Some("doc");
    }
    if config::CODE_EXTS.contains(e) {
        return Some("code");
    }
    if config::PROSE_EXTS.contains(e) {
        return Some("text");
    }
    if e.is_empty() {
        let base = config::file_name(name).to_lowercase();
        return config::KNOWN_NAMES.get(base.as_str()).copied();
    }
    None
}

/// What a file holds, by its first bytes: "image", "doc" or "text", or None for nothing the app can
/// read. This decides for files whose name doesn't (an unknown extension, or none).
pub fn sniff(path: &str) -> Option<&'static str> {
    let path = Path::new(path);
    let head = head(path, 4096)?;
    if head.is_empty() {
        return None;
    }
    if is_picture(&head) {
        return Some("image");
    }
    if head.starts_with(b"%PDF") || head.starts_with(b"{\\rtf") {
        return Some("doc");
    }
    if head.starts_with(OLE_MAGIC) {
        return ole_document(path).then_some("doc");
    }
    if head.starts_with(b"PK\x03\x04") {
        return zip_content(path);
    }
    let bom = head.starts_with(b"\xef\xbb\xbf") || head.starts_with(b"\xff\xfe") || head.starts_with(b"\xfe\xff");
    if !bom {
        if head.contains(&0) {
            return None;
        }
        // Text has hardly any control characters, whatever it's written in.
        let control = head.iter().filter(|&&b| (b < 32 && ![9, 10, 12, 13].contains(&b)) || b == 127).count();
        if control > head.len() / 100 {
            return None;
        }
    }
    Some(if looks_like_svg(&head) { "image" } else { "text" }) // (a drawing is shown as the picture it is)
}

/// A picture's signature: JPEG, PNG, GIF, TIFF, PSD, WebP, BMP, HEIC or AVIF.
fn is_picture(head: &[u8]) -> bool {
    let starts = [&b"\xff\xd8\xff"[..], b"\x89PNG\r\n\x1a\n", b"GIF87a", b"GIF89a", b"II*\x00", b"MM\x00*", b"8BPS"];
    let ftyp = [&b"heic"[..], b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1", b"avif", b"avis"];
    starts.iter().any(|s| head.starts_with(s))
        || (head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP")
        || (head.len() >= 12 && &head[4..8] == b"ftyp" && ftyp.contains(&&head[8..12]))
        // BMP: "BM", then the file size and two reserved words that are zero
        || (head.len() >= 26 && head.starts_with(b"BM") && head[6..10] == [0, 0, 0, 0] && head[14] >= 12 && head[15..18] == [0, 0, 0])
}

/// An SVG drawing: XML (perhaps after a declaration and comments) whose root is <svg>.
pub fn looks_like_svg(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with("<svg") || t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE svg"))
        && t.contains("<svg")
}

/// A web page saved under a name that doesn't say so.
fn looks_like_html(text: &str) -> bool {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    let start: String = t.chars().take(15).collect::<String>().to_lowercase();
    start.starts_with("<!doctype html") || start.starts_with("<html")
}

/// Office 97-2003 (Word, Excel, PowerPoint) or an Outlook e-mail, by the parts inside.
fn ole_document(path: &Path) -> bool {
    let Ok(file) = cfb::open(path) else { return false };
    let names: Vec<String> = file.read_root_storage().map(|e| e.name().to_string()).collect();
    names.iter().any(|n| matches!(n.as_str(), "WordDocument" | "Workbook" | "Book" | "PowerPoint Document") || n.starts_with("__substg1.0_"))
}

/// An Outlook e-mail (.msg), by the parts inside.
fn is_outlook(path: &Path) -> bool {
    head(path, 8).as_deref() == Some(OLE_MAGIC)
        && cfb::open(path).is_ok_and(|f| f.read_root_storage().any(|e| e.name().starts_with("__substg1.0_")))
}

/// What a ZIP-based file is, by the names inside: a document (Office 2007 and later,
/// OpenDocument, EPUB), a picture (a file that carries a picture of itself, like a layered
/// painting), or None (the app doesn't look inside archives it doesn't know by name).
fn zip_content(path: &Path) -> Option<&'static str> {
    let z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let names: std::collections::HashSet<&str> = z.file_names().collect();
    let office = names.contains("[Content_Types].xml")
        && names.iter().any(|n| n.starts_with("word/") || n.starts_with("xl/") || n.starts_with("ppt/"));
    if office || (names.contains("mimetype") && (names.contains("content.xml") || names.contains("META-INF/container.xml"))) {
        return Some("doc");
    }
    crate::pictures::PREVIEWS.iter().any(|p| names.contains(p)).then_some("image")
}

/// Why a file of a type that isn't searched isn't: "media", "program", "mailbox" or "type".
pub fn why_not(name: &str) -> &'static str {
    let ext = suffix(name);
    let e = ext.as_str();
    if config::MEDIA_EXTS.contains(e) {
        "media"
    } else if config::PROGRAM_EXTS.contains(e) {
        "program"
    } else if config::MAILBOX_EXTS.contains(e) {
        "mailbox"
    } else {
        "type"
    }
}

static QUOTED_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"][A-Za-z]:[\\/][^'"]*['"]"#).unwrap());

/// Why a file couldn't be read: "!key" where it's a known case (the page puts it in words), else
/// the error itself, without file paths.
pub fn problem(e: &ReadError) -> String {
    match e {
        ReadError::Skip(k) => format!("!{k}"),
        ReadError::Damaged(k) => format!("!damaged_{k}"),
        ReadError::Io(io) => {
            match io.raw_os_error() {
                Some(32) | Some(33) => return "!in_use".into(), // another program has it open, and locked
                Some(8) | Some(14) => return "!no_memory".into(),
                _ => {}
            }
            match io.kind() {
                std::io::ErrorKind::PermissionDenied => "!no_access".into(),
                std::io::ErrorKind::NotFound => "!gone".into(),
                std::io::ErrorKind::OutOfMemory => "!no_memory".into(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => "!damaged_document".into(),
                _ => clean_message("OSError", &io.to_string()),
            }
        }
        ReadError::Other(err) => {
            let text = format!("{err:#}");
            let lower = text.to_lowercase();
            if err.downcast_ref::<image::ImageError>().is_some() {
                return "!damaged_image".into();
            }
            if err.downcast_ref::<zip::result::ZipError>().is_some()
                || err.downcast_ref::<quick_xml::Error>().is_some()
                || err.downcast_ref::<calamine::Error>().is_some()
            {
                return "!damaged_document".into();
            }
            if lower.contains("out of memory") || lower.contains("memory allocation") {
                return "!no_memory".into();
            }
            clean_message("Error", &text)
        }
    }
}

fn clean_message(name: &str, text: &str) -> String {
    let t = QUOTED_PATH.replace_all(text, "");
    let s = format!("{name}: {t}");
    s.trim().chars().take(300).collect()
}

/// The biggest file of this kind that gets indexed.
pub fn max_bytes(kind: &str, name: &str) -> u64 {
    let ext = suffix(name);
    if kind == "image" && (config::RAW_EXTS.contains(ext.as_str()) || !config::IMAGE_EXTS.contains(ext.as_str())) {
        return config::MAX_RAW_BYTES; // only the picture inside is read (camera raw, a painting's, ...)
    }
    match kind {
        "image" => config::MAX_IMAGE_BYTES,
        "doc" => config::MAX_DOC_BYTES,
        _ => config::MAX_TEXT_BYTES,
    }
}

/// Whether a file can be shown as a picture: images, and documents with pages to show (a PDF
/// under another name too, when its path is given).
pub fn has_preview(path: &str, kind: &str) -> bool {
    let ext = suffix(path);
    kind == "image"
        || crate::paged::PREVIEW_EXTS.contains(&ext.as_str())
        || THUMBNAIL_EXTS.contains(&ext.as_str())
        || (kind == "doc" && !config::DOC_EXTS.contains(ext.as_str()) && crate::paged::is_pdf(Path::new(path)))
}

/// title: what the model sees as the document title (the file name unless given). kind: how to
/// read it, when not by its name (types added in Settings are read as text).
pub fn extract(path: &Path, size: u64, title: Option<&str>, kind: Option<&str>) -> ReadResult<Extracted> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let kind = match kind.or_else(|| kind_of(&name)).or_else(|| sniff(&path.to_string_lossy())) {
        Some(k) => k.to_string(),
        None => return Err(ReadError::Skip("binary")),
    };
    let ext = suffix(&name);
    if size == 0 {
        return Err(ReadError::Skip("empty"));
    }
    if size > max_bytes(&kind, &name) {
        return Err(ReadError::Skip("too_large"));
    }
    if kind == "image" {
        let image = match crate::pictures::load_image(path, config::MAX_IMAGE_SIDE) {
            Ok(img) => img,
            Err(ReadError::Damaged("image")) | Err(ReadError::Other(_))
                if config::IMAGE_EXTS.contains(ext.as_str()) && head(path, 4).as_deref() == Some(b"PK\x03\x04") =>
            {
                // an archive with a picture's name, e.g. an animated sticker
                return Err(ReadError::Skip("not_image"));
            }
            Err(e) => return Err(e),
        };
        return Ok(Extracted { kind, chunks: vec![Chunk { snippet: String::new(), text: None, image: Some(image) }] });
    }
    let text = if kind == "doc" {
        let (text, pages) = read_document(path)?;
        if text.trim().is_empty() && !pages.is_empty() {
            let chunks = pages
                .into_iter()
                .enumerate()
                .map(|(i, img)| Chunk { snippet: format!("Page {} (scanned)", i + 1), text: None, image: Some(img) })
                .collect();
            return Ok(Extracted { kind, chunks });
        }
        text
    } else {
        let text = read_text(path)?;
        if ext == ".ipynb" {
            notebook_text(&text)
        } else if HTML_EXTS.contains(&ext.as_str()) || (kind_of(&name).is_none() && looks_like_html(&text)) {
            strip_html(&text)
        } else {
            text
        }
    };
    let title = title.map(str::to_string).unwrap_or(name);
    let chunks: Vec<Chunk> = chunk_text(&text)
        .into_iter()
        .map(|c| Chunk { text: Some(format!("title: {title} | text: {c}")), snippet: c, image: None })
        .collect();
    if chunks.is_empty() {
        return Err(ReadError::Skip("no_text"));
    }
    Ok(Extracted { kind, chunks })
}

pub fn head(path: &Path, n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; n];
    let mut total = 0;
    while total < n {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(k) => total += k,
            Err(_) => return None,
        }
    }
    buf.truncate(total);
    Some(buf)
}

/// A document's text, or for one without any (a scan), its first pages as pictures.
pub fn read_document(path: &Path) -> ReadResult<(String, Vec<RgbImage>)> {
    let ext = suffix(&path.to_string_lossy());
    if crate::paged::has_pages(path) {
        return crate::paged::read_paged(path); // a PDF (whatever its name) or an EPUB book
    }
    let outlook = is_outlook(path);
    if outlook || matches!(ext.as_str(), ".eml" | ".msg" | ".mht" | ".mhtml") {
        if ext == ".msg" && !outlook {
            return Ok((plain_text(path)?, vec![])); // not an Outlook email: other programs use .msg too
        }
        let data = std::fs::read(path)?;
        let mut m = crate::mail::open_mail(&data).map_err(|_| ReadError::Damaged("email"))?;
        if matches!(ext.as_str(), ".mht" | ".mhtml") {
            m.attachments.clear(); // a saved web page's pictures and styles
        }
        return Ok((m.text(), vec![]));
    }
    Ok((crate::documents::read(path)?, vec![]))
}

pub fn read_text(path: &Path) -> ReadResult<String> {
    decode_text(&std::fs::read(path)?)
}

pub fn decode_text(data: &[u8]) -> ReadResult<String> {
    if data.starts_with(b"\xff\xfe\x00\x00") || data.starts_with(b"\x00\x00\xfe\xff") {
        return Ok(decode_utf32(data));
    }
    if data.starts_with(b"\xff\xfe") || data.starts_with(b"\xfe\xff") {
        // UTF-16, like Windows' .reg and .inf files
        let enc = if data[0] == 0xff { encoding_rs::UTF_16LE } else { encoding_rs::UTF_16BE };
        return Ok(enc.decode_with_bom_removal(data).0.into_owned());
    }
    if data[..data.len().min(8192)].contains(&0) {
        return Err(ReadError::Skip("binary"));
    }
    let body = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    match std::str::from_utf8(body) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Ok(encoding_rs::WINDOWS_1252.decode_without_bom_handling(data).0.into_owned()),
    }
}

fn decode_utf32(data: &[u8]) -> String {
    let le = data.starts_with(b"\xff\xfe\x00\x00");
    data[4..]
        .chunks_exact(4)
        .map(|c| {
            let v = if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
            char::from_u32(v).unwrap_or('\u{fffd}')
        })
        .collect()
}

/// A file read as text, without tags if it's a web page.
pub fn plain_text(path: &Path) -> ReadResult<String> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)?.take(config::MAX_TEXT_BYTES).read_to_end(&mut data)?;
    let text = decode_text(&data)?;
    Ok(if text.trim_start().starts_with('<') { strip_html(&text) } else { text })
}

pub fn notebook_text(raw: &str) -> String {
    let Ok(nb) = serde_json::from_str::<serde_json::Value>(raw) else { return raw.to_string() };
    let mut parts = Vec::new();
    for cell in nb.get("cells").and_then(|c| c.as_array()).into_iter().flatten() {
        match cell.get("source") {
            Some(serde_json::Value::Array(lines)) => {
                parts.push(lines.iter().filter_map(|l| l.as_str()).collect::<String>())
            }
            Some(serde_json::Value::String(s)) => parts.push(s.clone()),
            Some(other) => parts.push(other.to_string()),
            None => parts.push(String::new()),
        }
    }
    parts.join("\n\n")
}

static SCRIPT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<script\b.*?</script>").unwrap());
static STYLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<style\b.*?</style>").unwrap());
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

pub fn strip_html(raw: &str) -> String {
    let s = SCRIPT_RE.replace_all(raw, " ");
    let s = STYLE_RE.replace_all(&s, " ");
    let s = TAG_RE.replace_all(&s, " ");
    html_escape::decode_html_entities(&s).into_owned()
}

// ---- chunking ------------------------------------------------------------

/// Whitespace: Unicode's, and the file, group, record and unit separators.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn strip_chars(s: &[char]) -> &[char] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && is_space(s[a]) {
        a += 1;
    }
    while b > a && is_space(s[b - 1]) {
        b -= 1;
    }
    &s[a..b]
}

/// Whitespace as the chunker cleans it: line ends unified, tabs as 4 spaces, no spaces at line
/// ends, runs of spaces inside a line as one (leading indentation kept), at most one blank line,
/// none at the start or the end. In one pass, and only as far as the chunker can ever get (its
/// MAX_CHUNKS_PER_FILE windows): a long text costs no more than that.
fn tidy(text: &str) -> Vec<char> {
    let limit = config::MAX_CHUNKS_PER_FILE * config::CHUNK_CHARS + config::CHUNK_CHARS;
    let mut out: Vec<char> = Vec::with_capacity(text.len().min(limit));
    let mut pending: Vec<char> = Vec::new(); // whitespace since the last other character (not line ends)
    let mut newlines = 0; // line ends in a row at the end of `out`
    let mut chars = text.chars().peekable();
    while let Some(mut c) = chars.next() {
        if out.len() >= limit {
            return out; // (what's left can't reach a passage)
        }
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            c = '\n';
        }
        if c == '\n' {
            pending.clear(); // (spaces at a line's end go)
            newlines += 1;
            if newlines <= 2 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        if is_space(c) {
            if c == '\t' {
                pending.extend([' '; 4]);
            } else {
                pending.push(c);
            }
            continue;
        }
        if !out.is_empty() {
            if pending.len() >= 2 && out.last().is_some_and(|&p| !is_space(p)) {
                out.push(' ');
            } else {
                out.extend_from_slice(&pending);
            }
        }
        pending.clear();
        newlines = 0;
        out.push(c);
    }
    while out.last().is_some_and(|&c| is_space(c)) {
        out.pop();
    }
    out
}

fn rfind(window: &[char], sep: &[char]) -> Option<usize> {
    if sep.len() > window.len() {
        return None;
    }
    (0..=window.len() - sep.len()).rev().find(|&i| window[i..i + sep.len()] == *sep)
}

pub fn chunk_text(text: &str) -> Vec<String> {
    chunks_of(&tidy(text))
}

/// Passages of a tidied text: windows of CHUNK_CHARS, each ending at the strongest boundary in
/// its back half, overlapping by CHUNK_OVERLAP.
fn chunks_of(text: &[char]) -> Vec<String> {
    if text.is_empty() {
        return vec![];
    }
    let (size, overlap) = (config::CHUNK_CHARS, config::CHUNK_OVERLAP);
    let n = text.len();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let seps: [&[char]; 4] = [&['\n', '\n'], &['\n'], &['.', ' '], &[' ']];
    while start < n && chunks.len() < config::MAX_CHUNKS_PER_FILE {
        let mut end = (start + size).min(n);
        if end < n {
            // Break at the strongest boundary in the back half of the window.
            let window = &text[start..end];
            for sep in seps {
                if let Some(cut) = rfind(window, sep) {
                    if cut > size / 2 {
                        end = start + cut + sep.len();
                        break;
                    }
                }
            }
        }
        let chunk = strip_chars(&text[start..end]);
        if !chunk.is_empty() {
            chunks.push(chunk.iter().collect());
        }
        if end >= n {
            break;
        }
        start = end - overlap;
        if let Some(k) = text[start..end].iter().position(|&c| c == ' ') {
            start += k + 1;
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whitespace as the chunker cleans it: line ends unified, tabs as 4 spaces, no spaces at line
    /// ends, runs of spaces inside a line as one (leading indentation kept), at most one blank line.
    fn tidy_reference(text: &str) -> Vec<char> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        let chars: Vec<char> = text.chars().collect();
        // [^\S\n]+\n -> \n
        let mut a: Vec<char> = Vec::with_capacity(chars.len());
        let mut run = 0usize;
        for &c in &chars {
            if c == '\n' {
                a.truncate(a.len() - run);
                a.push('\n');
                run = 0;
            } else {
                a.push(c);
                run = if is_space(c) { run + 1 } else { 0 };
            }
        }
        // (?<=\S)[^\S\n]{2,} -> " "
        let mut b: Vec<char> = Vec::with_capacity(a.len());
        let mut i = 0;
        while i < a.len() {
            let c = a[i];
            if is_space(c) && c != '\n' && i > 0 && !is_space(a[i - 1]) {
                let mut j = i;
                while j < a.len() && is_space(a[j]) && a[j] != '\n' {
                    j += 1;
                }
                if j - i >= 2 {
                    b.push(' ');
                    i = j;
                    continue;
                }
            }
            b.push(c);
            i += 1;
        }
        // \n{3,} -> \n\n
        let mut c2: Vec<char> = Vec::with_capacity(b.len());
        let mut nl = 0;
        for &c in &b {
            if c == '\n' {
                nl += 1;
                if nl <= 2 {
                    c2.push(c);
                }
            } else {
                nl = 0;
                c2.push(c);
            }
        }
        strip_chars(&c2).to_vec()
    }

    #[test]
    fn tidying_in_one_pass_matches_the_reference_and_long_texts_give_the_same_passages() {
        let pieces: Vec<String> = ["word", "Wort", "x", ".", " ", "  ", "\t", "\n", "\r\n", "\r", "\n\n\n"]
            .iter()
            .map(|s| s.to_string())
            .chain([0xA0u32, 0x2003, 0x1C, 0x0B].iter().map(|&c| char::from_u32(c).unwrap().to_string()))
            .collect();
        let mut seed = 7u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for len in [0, 1, 2, 5, 20, 200, 3000] {
            for _ in 0..200 {
                let text: String = (0..len).map(|_| pieces[next(pieces.len())].as_str()).collect();
                assert_eq!(tidy(&text), tidy_reference(&text), "{text:?}");
            }
        }
        // Longer than the passages can reach: the same passages as from the whole text.
        let long: String = (0..700_000).map(|_| pieces[next(pieces.len())].as_str()).collect();
        assert!(long.chars().count() > 2 * config::MAX_CHUNKS_PER_FILE * config::CHUNK_CHARS);
        let all = chunks_of(&tidy_reference(&long));
        assert_eq!(all.len(), config::MAX_CHUNKS_PER_FILE);
        assert_eq!(chunk_text(&long), all);
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk_text("  Hello   world  \r\n\r\n\r\n\r\nBye\t "), vec!["Hello world\n\nBye"]);
    }

    #[test]
    fn long_text_breaks_at_paragraphs_with_overlap() {
        let para = "word ".repeat(200);
        let text = format!("{para}\n\n{para}\n\n{para}");
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= config::CHUNK_CHARS));
    }

    #[test]
    fn kinds_by_name() {
        assert_eq!(kind_of("a.PDF"), Some("doc"));
        assert_eq!(kind_of("x.rs"), Some("code"));
        assert_eq!(kind_of("Makefile"), Some("code"));
        assert_eq!(kind_of("README"), Some("text"));
        assert_eq!(kind_of("song.mp3"), None);
        assert_eq!(why_not("song.mp3"), "media");
    }

    #[test]
    fn html_loses_its_tags_and_scripts() {
        assert_eq!(strip_html("<p>a&amp;b</p><script>x()</script>").trim(), "a&b");
    }
}
