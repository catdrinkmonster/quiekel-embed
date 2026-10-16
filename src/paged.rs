//! Documents read page by page: PDF (known by its content, so also under other names, like
//! Illustrator's .ai) and EPUB books. Pages without text (scans) are looked at instead.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use image::RgbImage;
use pdfium_render::prelude::*;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use regex::Regex;

use crate::config::{self, suffix};
use crate::extract::{MAX_CHARS, ReadError, ReadResult};
use crate::pictures;

/// Files that can be shown as a picture of their first page (and, by content, any PDF).
pub const PREVIEW_EXTS: [&str; 2] = crate::extract::PAGED_EXTS;
/// Where each page starts in a PDF's text, so passages know their page (extract::chunk_pages): a
/// character from the private use area, which the cleaned text never holds.
pub const PAGE_MARK: char = '\u{e000}';
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

/// A book's text by page (or chapter), and where its pictures are, first ones first.
struct Book {
    pages: Vec<String>,
    pictures: Vec<Vec<u8>>, // the first few, for books without text
    cover: Option<Vec<u8>>,
    first: Option<Vec<u8>>, // the first page's picture, when it shows one
}

/// MuPDF's pixel size for a length in points: up to the next whole pixel.
pub fn pixels(points: f64) -> u32 {
    ((points - 0.001).ceil().max(1.0)) as u32
}

/// Whether a file is a PDF inside, whatever its name says.
pub fn is_pdf(path: &Path) -> bool {
    crate::extract::head(path, 5).is_some_and(|h| h.starts_with(b"%PDF"))
}

/// Whether a file is an EPUB book: a ZIP whose first entry, "mimetype", says so.
pub fn is_epub(path: &Path) -> bool {
    crate::extract::head(path, 128)
        .is_some_and(|h| h.starts_with(b"PK\x03\x04") && h.get(30..38) == Some(b"mimetype") && memchr::memmem::find(&h, b"application/epub+zip").is_some())
}

/// Documents read page by page: PDFs (by content) and EPUB books.
pub fn has_pages(path: &Path) -> bool {
    let ext = suffix(&path.to_string_lossy());
    ext == ".pdf" || ext == ".epub" || is_pdf(path) || is_epub(path)
}

/// The text, and for a document without any (a scan), its first pages as pictures.
pub fn read_paged(path: &Path) -> ReadResult<(String, Vec<RgbImage>)> {
    if is_pdf(path) || suffix(&path.to_string_lossy()) == ".pdf" {
        return read_pdf(path);
    }
    let book = epub(path)?;
    let mut parts = Vec::new();
    let mut total = 0;
    for p in book.pages {
        total += p.chars().count();
        parts.push(p);
        if total > MAX_CHARS {
            break;
        }
    }
    let text = parts.join("\n\n");
    let mut pages = Vec::new();
    if text.trim().is_empty() {
        for data in book.pictures.iter().take(config::SCANNED_PDF_PAGES) {
            if let Ok(img) = pictures::decode(data) {
                pages.push(pictures::thumbnail(&img, config::MAX_IMAGE_SIDE));
            }
        }
    }
    Ok((text, pages))
}

/// How many pages a PDF has (None: not a PDF, or one that can't be opened).
pub fn page_count(path: &Path) -> Option<u32> {
    if !(is_pdf(path) || suffix(&path.to_string_lossy()) == ".pdf") {
        return None;
    }
    let (_one, pdfium) = pdfium().ok()?; // (dropped last, after the document)
    let doc = open_pdf(pdfium, path).ok()?;
    let n = doc.pages().len() as u32;
    Some(n)
}

/// The first page as a picture, `size` pixels on its longer side (for previews).
pub fn first_page(path: &Path, size: u32) -> ReadResult<RgbImage> {
    page_picture(path, 1, size)
}

/// A page as a picture (from 1; past the last, the last), `size` pixels on its longer side. A book
/// shows its first page.
pub fn page_picture(path: &Path, number: u32, size: u32) -> ReadResult<RgbImage> {
    if is_pdf(path) || suffix(&path.to_string_lossy()) == ".pdf" {
        let (_one, pdfium) = pdfium()?; // (dropped last, after the document)
        let doc = open_pdf(pdfium, path)?;
        let pages = doc.pages();
        let index = (number.max(1) - 1).min((pages.len() as u32).saturating_sub(1)) as u16;
        let page = pages.get(index).map_err(|_| ReadError::Skip("nothing_to_show"))?;
        let zoom = size as f64 / (page.width().value.max(page.height().value) as f64).max(1.0);
        return render(&page, zoom);
    }
    let book = epub(path)?;
    if let Some(data) = book.first.clone() {
        // (its first page, as laid out)
        let img = pictures::decode(&data).map_err(|_| ReadError::Damaged("image"))?;
        return Ok(pictures::thumbnail(&img, size));
    }
    // A book of text: its first page, as a reader would lay it out.
    let text = book.pages.iter().find(|p| !p.trim().is_empty()).ok_or(ReadError::Skip("nothing_to_show"))?;
    pictures::text_page(text, size)
}

// ---- PDF ------------------------------------------------------------------------------------

/// PDFium, for one caller at a time in the whole app: it isn't thread-safe, and the binding's own
/// lock only keeps separate instances apart. Hold the guard until the document is dropped.
fn pdfium() -> ReadResult<(parking_lot::MutexGuard<'static, ()>, &'static Pdfium)> {
    static ONE_AT_A_TIME: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();
    let guard = ONE_AT_A_TIME.lock();
    let pdfium = PDFIUM
        .get_or_init(|| {
            let dll = config::vendor_file("pdfium", "pdfium.dll");
            Pdfium::bind_to_library(&dll).ok().map(Pdfium::new)
        })
        .as_ref()
        .ok_or_else(|| ReadError::Other(anyhow::anyhow!("PDFium is missing")))?;
    Ok((guard, pdfium))
}

fn open_pdf<'a>(pdfium: &'a Pdfium, path: &Path) -> ReadResult<PdfDocument<'a>> {
    pdfium.load_pdf_from_file(path, None).map_err(|e| match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => ReadError::Skip("password"),
        PdfiumError::IoError(io) => ReadError::Io(io),
        _ => ReadError::Damaged("pdf"),
    })
}

fn render(page: &PdfPage, zoom: f64) -> ReadResult<RgbImage> {
    let (w, h) = (pixels(page.width().value as f64 * zoom), pixels(page.height().value as f64 * zoom));
    let config = PdfRenderConfig::new().set_target_width(w as i32).set_target_height(h as i32);
    let bitmap = page.render_with_config(&config).map_err(|_| ReadError::Damaged("pdf"))?;
    Ok(bitmap.as_image().to_rgb8())
}

/// A page's text as PDFium gives it, made fit to search: the U+0002 it puts where a word was split
/// at a line end goes (the halves are one word: "ge\u{2}worfen"), other control codes and the
/// glyphs of symbol fonts (the private use area: bullets, arrows, no letters) become spaces.
fn clean_pdf_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n").replace("\u{2}\n", "");
    text.chars()
        .filter(|&c| c != '\u{2}')
        .map(|c| if (c.is_control() && c != '\n' && c != '\t') || ('\u{e000}'..='\u{f8ff}').contains(&c) { ' ' } else { c })
        .collect()
}

/// Whether there's anything to read: a letter or a digit somewhere. A PDF without (a scan, or one
/// whose fonts don't say which letters they show) is looked at instead.
fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

fn read_pdf(path: &Path) -> ReadResult<(String, Vec<RgbImage>)> {
    let (_one, pdfium) = pdfium()?; // (dropped last, after the document)
    let doc = open_pdf(pdfium, path)?;
    let pages = doc.pages();
    let mut parts = Vec::new();
    let mut total = 0;
    for page in pages.iter() {
        let mut text = clean_pdf_text(&page.text().map(|t| t.all()).unwrap_or_default());
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.insert(0, PAGE_MARK); // (where this page starts)
        total += text.chars().count();
        parts.push(text);
        if total > MAX_CHARS {
            break;
        }
    }
    let mut text = parts.join("\n\n");
    let mut images = Vec::new();
    if !has_words(&text) {
        text.clear();
        let n = (pages.len() as usize).min(config::SCANNED_PDF_PAGES);
        for i in 0..n {
            let page = pages.get(i as u16).map_err(|_| ReadError::Damaged("pdf"))?;
            let img = render(&page, 100.0 / 72.0)?;
            images.push(pictures::thumbnail(&img, config::MAX_IMAGE_SIDE));
        }
    }
    Ok((text, images))
}

// ---- shared: ZIP packages and markup ---------------------------------------------------------

type Zip = zip::ZipArchive<std::fs::File>;

fn open_zip(path: &Path) -> ReadResult<Zip> {
    zip::ZipArchive::new(std::fs::File::open(path)?).map_err(|_| ReadError::Damaged("document"))
}

/// A part of the package by its name (case doesn't matter, as in many writers' output).
fn part(z: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let name = name.trim_start_matches('/');
    let index = z.index_for_name(name).or_else(|| {
        let lower = name.to_lowercase();
        z.file_names().position(|n| n.to_lowercase() == lower)
    })?;
    let f = z.by_index(index).ok()?;
    let mut out = Vec::new();
    f.take(MAX_PART_BYTES + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= MAX_PART_BYTES).then_some(out)
}

/// "OEBPS/text/ch1.xhtml" + "../images/a.jpg" -> "OEBPS/images/a.jpg".
fn join(base_file: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or("");
    let href = percent_decode(href);
    if let Some(abs) = href.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_file.split('/').collect();
    parts.pop();
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| local(a.key.as_ref()) == key).map(|a| {
        let raw = String::from_utf8_lossy(&a.value);
        quick_xml::escape::unescape(&raw).map(|v| v.into_owned()).unwrap_or_else(|_| raw.into_owned())
    })
}

/// Markup as bytes decoded by what it says it is (an XML declaration, a meta charset), else UTF-8.
fn decode_markup(data: &[u8]) -> String {
    static CHARSET: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(r#"(?i)(?:encoding|charset)\s*=\s*["']?([A-Za-z0-9_\-]+)"#).unwrap()
    });
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(data) {
        return enc.decode_without_bom_handling(&data[bom..]).0.into_owned();
    }
    let head = &data[..data.len().min(1024)];
    if let Some(c) = CHARSET.captures(head) {
        if let Some(enc) = encoding_rs::Encoding::for_label(&c[1]) {
            if enc != encoding_rs::UTF_8 {
                return enc.decode_without_bom_handling(data).0.into_owned();
            }
        }
    }
    String::from_utf8_lossy(data).into_owned()
}

static BLOCK_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)</(?:p|div|h[1-6]|li|tr|td|th|blockquote|pre|section|article|dt|dd|title)\s*>|<br\s*/?>|<hr\s*/?>").unwrap()
});
static IMG_SRC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<(?:img\b[^>]*?\bsrc|image\b[^>]*?\bhref)\s*=\s*["']([^"']+)["']"#).unwrap()
});

/// A web page's text, its paragraphs on lines of their own.
fn html_text(html: &str) -> String {
    let body = match html.to_lowercase().find("<body") {
        Some(i) => &html[i..],
        None => html,
    };
    let marked = BLOCK_END.replace_all(body, "\n");
    let text = crate::extract::strip_html(&marked);
    let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    let mut out = String::new();
    for l in lines.iter().filter(|l| !l.is_empty()) {
        out.push_str(l);
        out.push('\n');
    }
    out
}


// ---- EPUB -------------------------------------------------------------------------------------

fn epub(path: &Path) -> ReadResult<Book> {
    let mut z = open_zip(path)?;
    let container = part(&mut z, "META-INF/container.xml").ok_or(ReadError::Damaged("document"))?;
    let mut opf_path = None;
    let mut r = Reader::from_reader(container.as_slice());
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"rootfile" => {
                opf_path = attr(&e, b"full-path");
                break;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let opf_path = opf_path.ok_or(ReadError::Damaged("document"))?;
    let opf = part(&mut z, &opf_path).ok_or(ReadError::Damaged("document"))?;
    let mut manifest: HashMap<String, (String, String, String)> = HashMap::new(); // id -> (href, type, properties)
    let mut spine: Vec<String> = Vec::new();
    let mut cover_id = None;
    let mut r = Reader::from_reader(opf.as_slice());
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"item" => {
                    if let (Some(id), Some(href)) = (attr(&e, b"id"), attr(&e, b"href")) {
                        let kind = attr(&e, b"media-type").unwrap_or_default();
                        let props = attr(&e, b"properties").unwrap_or_default();
                        manifest.insert(id, (join(&opf_path, &href), kind, props));
                    }
                }
                b"itemref" => {
                    if let Some(id) = attr(&e, b"idref") {
                        spine.push(id);
                    }
                }
                b"meta" if attr(&e, b"name").as_deref() == Some("cover") => cover_id = attr(&e, b"content"),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let cover_path = manifest
        .values()
        .find(|(_, _, p)| p.split_whitespace().any(|x| x == "cover-image"))
        .map(|(h, _, _)| h.clone())
        .or_else(|| cover_id.and_then(|id| manifest.get(&id)).map(|(h, _, _)| h.clone()));
    let mut book = Book { pages: Vec::new(), pictures: Vec::new(), cover: None, first: None };
    let mut total = 0;
    for (n, id) in spine.iter().enumerate() {
        let Some((href, _, _)) = manifest.get(id) else { continue };
        let Some(data) = part(&mut z, href) else { continue };
        let html = decode_markup(&data);
        if n == 0 {
            // what the first page shows: its picture, if it has one (a cover page)
            book.first = IMG_SRC.captures(&html).and_then(|c| part(&mut z, &join(href, &c[1])));
        }
        if book.pictures.len() < config::SCANNED_PDF_PAGES {
            for c in IMG_SRC.captures_iter(&html) {
                let target = join(href, &c[1]);
                if book.pictures.len() < config::SCANNED_PDF_PAGES {
                    if let Some(p) = part(&mut z, &target) {
                        book.pictures.push(p);
                    }
                }
            }
        }
        if total <= MAX_CHARS {
            let text = html_text(&html);
            total += text.chars().count();
            book.pages.push(text);
        }
    }
    book.cover = cover_path.and_then(|p| part(&mut z, &p));
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_text() {
        assert_eq!(join("OEBPS/text/ch1.xhtml", "../images/a%20b.jpg#x"), "OEBPS/images/a b.jpg");
        assert_eq!(html_text("<html><body><p>One</p><p>Two  three</p></body></html>"), "One\nTwo three\n");
    }

    #[test]
    fn pdf_text_joins_split_words_and_drops_glyphs_without_letters() {
        // As PDFium gives it: U+0002 where a word was split at a line end, a symbol font's bullet
        // in the private use area, a control code for another one.
        let raw = "Ein Würfel wird ge\u{2}worfen.\r\n\u{f}Die Grundgesamtheit\r\n\u{f0b7} Bal\u{2}\r\nkon\tWetter";
        assert_eq!(clean_pdf_text(raw), "Ein Würfel wird geworfen.\n Die Grundgesamtheit\n  Balkon\tWetter");
        // Only bullets and full stops: nothing to read, so the pages are looked at instead.
        assert!(!has_words(&clean_pdf_text("\u{f0d3}\n\n\u{f0b7}\u{f0b7}\u{f0b7}\n.\n,\n.")));
        assert!(has_words("Seite 3") && has_words("3"));
    }

    #[test]
    fn pdfs_read_from_many_threads_at_once_come_out_whole() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
        let (lease, scan) = (fixtures.join("lease.pdf"), fixtures.join("scan.pdf"));
        let expected = read_paged(&lease).unwrap().0;
        std::thread::scope(|s| {
            for i in 0..8 {
                let (lease, scan, expected) = (&lease, &scan, &expected);
                s.spawn(move || {
                    for _ in 0..12 {
                        if i % 2 == 0 {
                            assert_eq!(&read_paged(lease).unwrap().0, expected);
                        } else {
                            assert!(first_page(scan, 200).is_ok() && !read_paged(scan).unwrap().1.is_empty());
                        }
                    }
                });
            }
        });
    }
}
