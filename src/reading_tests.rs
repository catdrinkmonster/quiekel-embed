//! Every kind of file the app reads (sample files in tests/fixtures, or made here).

use std::path::{Path, PathBuf};

use crate::extract::{self, ReadError, chunk_text, extract, has_preview, kind_of, problem};
use crate::filetypes::Types;
use crate::{config, documents, mail, pictures};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

/// A folder of the test's own, gone afterwards.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("quiekel-rs-read-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn file(&self, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(path: &Path) -> extract::ReadResult<extract::Extracted> {
    extract(path, std::fs::metadata(path).unwrap().len(), None, None)
}

fn text_of(path: &Path) -> String {
    let ex = read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    ex.chunks.iter().map(|c| c.snippet.as_str()).collect::<Vec<_>>().join("\n")
}

/// Why the file isn't searched (it must be skipped, not fail).
fn skipped(path: &Path) -> &'static str {
    match read(path) {
        Err(ReadError::Skip(key)) => key,
        Err(e) => panic!("{}: failed instead: {e}", path.display()),
        Ok(_) => panic!("{}: read", path.display()),
    }
}

fn failed(path: &Path) -> ReadError {
    match read(path) {
        Err(ReadError::Skip(key)) => panic!("{}: skipped ({key}) instead", path.display()),
        Err(e) => e,
        Ok(_) => panic!("{}: read", path.display()),
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(w, h, image::Rgb([240, 120, 40])).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn jpeg(w: u32, h: u32, color: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&image::RgbImage::from_pixel(w, h, image::Rgb(color)))
        .unwrap();
    out
}

fn zip_of(path: &Path, files: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut z, data).unwrap();
    }
    z.finish().unwrap();
}

// ---- passages ------------------------------------------------------------------------------------

#[test]
fn kinds_by_extension() {
    assert_eq!(kind_of("a.PDF"), Some("doc"));
    assert_eq!(kind_of("a.py"), Some("code"));
    assert_eq!(kind_of("notes.md"), Some("text"));
    assert_eq!(kind_of("IMG_1.HEIC"), Some("image"));
    assert_eq!(kind_of("movie.mkv"), None);
}

#[test]
fn short_text_is_one_chunk() {
    assert_eq!(chunk_text("hello   world\n\n\n\nbye"), ["hello world\n\nbye"]);
}

#[test]
fn windows_newlines_and_indentation() {
    let code = "def f():\r\n    return  1   \r\n\r\n\r\n\r\nx = 2\r\n";
    assert_eq!(chunk_text(code), ["def f():\n    return 1\n\nx = 2"]);
}

#[test]
fn long_text_chunks_cover_everything_with_overlap() {
    let words: Vec<String> = (0..3000).map(|i| format!("word{i}")).collect();
    let chunks = chunk_text(&words.join(" "));
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| c.chars().count() <= config::CHUNK_CHARS));
    let joined = chunks.join(" ");
    let all: std::collections::HashSet<&str> = joined.split(' ').collect();
    assert!(words.iter().all(|w| all.contains(w.as_str()))); // nothing dropped
    for pair in chunks.windows(2) {
        let first = pair[1].split(' ').next().unwrap();
        assert!(pair[0].split(' ').any(|w| w == first)); // consecutive chunks overlap
    }
}

#[test]
fn text_file_gets_title_prefix() {
    let d = Dir::new();
    let p = d.file("groceries.md", "Milk, eggs, bread");
    let ex = read(&p).unwrap();
    assert_eq!(ex.kind, "text");
    assert_eq!(ex.chunks[0].text.as_deref(), Some("title: groceries.md | text: Milk, eggs, bread"));
    assert_eq!(ex.chunks[0].snippet, "Milk, eggs, bread");
}

#[test]
fn binary_and_empty_files_are_skipped() {
    let d = Dir::new();
    skipped(&d.file("data.json", b"\x00\x01\x02"));
    skipped(&d.file("empty.txt", "   \n "));
}

// ---- pictures ------------------------------------------------------------------------------------

#[test]
fn pictures_are_made_smaller_and_icons_left_out() {
    let d = Dir::new();
    let big = d.0.join("photo.png");
    image::RgbaImage::from_pixel(3000, 2000, image::Rgba([255, 0, 0, 128])).save(&big).unwrap();
    let ex = read(&big).unwrap();
    let img = ex.chunks[0].image.as_ref().unwrap();
    assert_eq!(ex.kind, "image");
    assert_eq!(img.width().max(img.height()), config::MAX_IMAGE_SIDE);
    skipped(&d.file("icon.png", png(32, 32)));
}

#[test]
fn a_photo_missing_its_last_bytes_is_still_indexed() {
    let d = Dir::new();
    let full = jpeg(400, 300, [30, 120, 200]);
    let p = d.file("cut.jpg", &full[..full.len() - 4]); // an interrupted copy
    let ex = read(&p).unwrap();
    assert_eq!(ex.kind, "image");
    assert_eq!(ex.chunks[0].image.as_ref().unwrap().dimensions(), (400, 300));
}

#[test]
fn empty_pictures_are_skipped_quietly() {
    let d = Dir::new();
    skipped(&d.file("empty.jpg", b""));
}

#[test]
fn an_archive_named_like_a_picture_is_skipped_not_an_error() {
    let d = Dir::new();
    let p = d.0.join("sticker.webp"); // e.g. an animated sticker: a ZIP with an animation inside
    zip_of(&p, &[("animation/animation.json", b"{}")]);
    skipped(&p);
}

#[test]
fn a_broken_picture_is_still_reported() {
    let d = Dir::new();
    let p = d.file("stub.jpg", b"\xff\xd8\xff");
    assert_eq!(problem(&failed(&p)), "!damaged_image");
}

#[test]
fn problems_are_named_without_file_paths() {
    assert_eq!(problem(&ReadError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied))), "!no_access");
    let bad_zip = zip::ZipArchive::new(std::io::Cursor::new(b"not a zip".to_vec())).unwrap_err();
    assert_eq!(problem(&ReadError::Other(bad_zip.into())), "!damaged_document");
    let text = problem(&ReadError::Other(anyhow::anyhow!(r"cannot read 'C:\Users\me\Documents\secret.txt' at all")));
    assert!(!text.contains("Users") && text.starts_with("Error: cannot read"), "{text}");
    assert_eq!(problem(&ReadError::Damaged("email")), "!damaged_email");
    assert_eq!(problem(&ReadError::Io(std::io::Error::from_raw_os_error(32))), "!in_use"); // locked by another program
}

#[test]
fn svg_pictures_and_icons() {
    let d = Dir::new();
    let svg = d.file(
        "plan.svg",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="200"><rect width="300" height="200" fill="#f54e00"/></svg>"##,
    );
    let (w, h) = read(&svg).unwrap().chunks[0].image.as_ref().unwrap().dimensions();
    assert!(w == 1024 && h.abs_diff(683) <= 1, "{w} x {h}");
    let icon = d.file("icon.svg", r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"/>"#);
    assert_eq!(skipped(&icon), "tiny_image");
}

#[test]
fn camera_raw_files_are_seen_by_their_preview() {
    let ex = read(&fixture("DSC_0042.NEF")).unwrap();
    assert_eq!(ex.kind, "image");
    assert_eq!(ex.chunks[0].image.as_ref().unwrap().dimensions(), (600, 400));
}

#[test]
fn layered_paintings_are_seen_by_their_finished_picture() {
    let d = Dir::new();
    let (merged, preview) = (png(640, 480), png(256, 192));
    for name in ["Drache.kra", "Skizze.ora"] {
        // Krita, OpenRaster: ZIPs with the merged picture inside
        let painting = d.0.join(name);
        zip_of(&painting, &[("mimetype", b"application/x-krita"), ("mergedimage.png", &merged), ("preview.png", &preview)]);
        let ex = read(&painting).unwrap();
        assert_eq!(ex.kind, "image");
        assert_eq!(ex.chunks[0].image.as_ref().unwrap().dimensions(), (640, 480));
        assert_eq!(pictures::picture(&painting, 320).unwrap().dimensions(), (320, 240));
    }
    let broken = d.file("kaputt.kra", b"PK\x03\x04 not really");
    assert_eq!(skipped(&broken), "binary"); // (its name says nothing, and neither does what's inside)
}

#[test]
fn files_without_an_extension_are_looked_at() {
    let d = Dir::new();
    let photo = d.file("441380981_837794958383993_n", jpeg(640, 480, [200, 80, 40])); // how some exports name photos
    let note = d.file("Einkaufsliste", "Milch, Eier, Mehl");
    let blob = d.file("cache_0001", (0..4).flat_map(|_| 0..=255u8).collect::<Vec<u8>>());
    let types = Types::new(&[], &Default::default());
    let path = |p: &Path| p.to_string_lossy().to_string();
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(types.kind(&name(&photo), Some(path(&photo).as_str())), Some("image"));
    assert_eq!(types.kind(&name(&note), Some(path(&note).as_str())), Some("text"));
    assert_eq!(types.kind(&name(&blob), Some(path(&blob).as_str())), None);
    assert_eq!(types.kind(&name(&photo), None), None); // (the name alone says nothing)
    let ex = extract(&photo, std::fs::metadata(&photo).unwrap().len(), None, Some("image")).unwrap();
    assert_eq!(ex.chunks[0].image.as_ref().unwrap().dimensions(), (640, 480));
}

// ---- documents -----------------------------------------------------------------------------------

#[test]
fn word_and_pdf_documents() {
    assert!(read(&fixture("budget.docx")).unwrap().chunks[0].snippet.contains("Quarterly budget review"));
    assert!(read(&fixture("lease.pdf")).unwrap().chunks[0].snippet.contains("Rental agreement"));
    let scan = read(&fixture("scan.pdf")).unwrap(); // a page without a text layer
    assert!(scan.chunks[0].image.is_some() && scan.chunks[0].snippet.starts_with("Page 1"));
}

#[test]
fn libreoffice_text_with_its_spaces_tables_and_footnotes() {
    let text = text_of(&fixture("Mietvertrag.odt"));
    assert!(text.contains("Die Miete beträgt 800 Euro monatlich."), "{text}"); // (runs of spaces collapse when chunked)
    assert!(text.contains("Ohne Nebenkosten") && text.contains("Kaution | 2400"));
    assert!(!text.contains("alter Betrag")); // deleted in tracked changes
}

#[test]
fn libreoffice_spreadsheets_and_presentations() {
    assert!(documents::read(&fixture("Haushalt.ods")).unwrap().contains("Sheet März:\nStrom | 59,50"));
    assert!(documents::read(&fixture("Urlaub.odp")).unwrap().contains("Slide 1:\nItalien 2026"));
}

#[test]
fn libreoffice_documents_with_a_password_are_skipped() {
    assert_eq!(skipped(&fixture("geheim.odt")), "password");
}

#[test]
fn flat_opendocument() {
    assert_eq!(text_of(&fixture("notiz.fodt")), "Flache Datei");
}

#[test]
fn libreoffice_documents_show_their_own_preview() {
    assert!(has_preview("mit_bild.odt", "doc"));
    assert_eq!(pictures::picture(&fixture("mit_bild.odt"), 360).unwrap().dimensions(), (256, 180));
}

#[test]
fn word_macro_documents_and_templates() {
    for name in ["a.docm", "a.dotx"] {
        assert_eq!(text_of(&fixture(name)), "Angebot Dachsanierung", "{name}");
    }
}

#[test]
fn powerpoint_slide_shows_with_their_notes() {
    let text = text_of(&fixture("u.ppsx"));
    assert!(text.contains("Urlaubsplanung") && text.contains("Notes: Hund nicht vergessen"), "{text}");
}

#[test]
fn word_97_with_its_fields() {
    assert_eq!(text_of(&fixture("Brief.doc")), "Sehr geehrte Frau Beispiel,\ndie Miete steigt ab Mai.");
}

#[test]
fn excel_97() {
    assert_eq!(text_of(&fixture("Haushalt.xls")), "Sheet Kosten:\nMiete | 800\nStrom | 59.5");
}

#[test]
fn powerpoint_97_without_its_master_slides() {
    let text = text_of(&fixture("Vortrag.ppt"));
    assert!(text.contains("Slide 1:\nUrlaubsplanung") && text.contains("Slide 2:\nStrand und Berge"), "{text}");
    assert!(!text.contains("Master"));
}

#[test]
fn an_old_office_name_with_something_else_inside() {
    let d = Dir::new();
    let rtf = d.file("Brief.doc", br"{\rtf1\ansi Hallo \'e4\par Welt}"); // Word saves RTF like this too
    assert_eq!(text_of(&rtf), "Hallo ä\nWelt");
    let page = d.file("Tabelle.xls", "<html><body><table><tr><td>Umsatz</td></tr></table></body></html>"); // Excel's web pages
    assert_eq!(text_of(&page), "Umsatz");
}

#[test]
fn rtf() {
    assert_eq!(text_of(&fixture("Brief.rtf")), "Grüße aus Köln\nPreis: 5€ nettoLink\nEnde");
}

#[test]
fn ebooks() {
    assert!(text_of(&fixture("Roman.epub")).contains("Es war einmal ein Schwein."));
    assert!(has_preview("Roman.epub", "doc"));
}

#[test]
fn illustrator_drawings_are_read_as_the_pdfs_they_are() {
    let ai = fixture("Logo.ai");
    assert!(text_of(&ai).contains("Schmidt bakery"));
    assert!(has_preview(&ai.to_string_lossy(), "doc") && pictures::picture(&ai, 200).unwrap().width() > 0);
}

#[test]
fn text_in_utf16_and_files_known_by_name() {
    let d = Dir::new();
    let text = "Windows Registry Editor Version 5.00\r\n[HKEY_CURRENT_USER\\Test]";
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
    let reg = d.file("export.reg", utf16);
    assert!(text_of(&reg).starts_with("Windows Registry Editor"));
    assert_eq!((kind_of("Makefile"), kind_of("README"), kind_of("notes")), (Some("code"), Some("text"), None));
}

#[test]
fn every_type_in_the_lists_has_a_reader() {
    let lists = [&*config::DOC_EXTS, &*config::PROSE_EXTS, &*config::CODE_EXTS, &*config::IMAGE_EXTS];
    for ext in lists.into_iter().flatten() {
        assert!(kind_of(&format!("file{ext}")).is_some(), "{ext}");
    }
}

#[test]
fn broken_files_are_named_in_words() {
    let d = Dir::new();
    let bad = d.file("kaputt.doc", b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1 not really");
    assert_eq!(problem(&failed(&bad)), "!damaged_document");
}

// ---- emails ----------------------------------------------------------------------------------------

#[test]
fn emails_saved_from_outlook() {
    let text = text_of(&fixture("Rechnung.msg"));
    assert!(text.starts_with("Subject: Ihre Rechnung für März\nFrom: Stadtwerke <info@stadtwerke.example>"), "{text}");
    assert!(text.contains("To: Max Muster") && text.contains("Date: 2026-03-03"));
    assert!(text.contains("Attachments: rechnung.txt") && text.ends_with("Anbei die Rechnung."));
}

#[test]
fn a_msg_file_that_isnt_an_email() {
    let d = Dir::new();
    let catalog = d.file("de.msg", "::msgcat::mcset de Hello Hallo"); // other programs use .msg for their own text
    assert_eq!(text_of(&catalog), "::msgcat::mcset de Hello Hallo");
}

#[test]
fn emails_and_saved_web_pages_in_the_standard_format() {
    let d = Dir::new();
    let eml = d.file(
        "Termin.eml",
        b"From: Praxis <praxis@example.com>\r\nTo: max@example.com\r\n\
          Subject: =?utf-8?q?Terminbest=C3=A4tigung?=\r\nDate: Tue, 03 Mar 2026 10:15:00 +0100\r\n\
          Content-Type: text/html; charset=utf-8\r\n\r\n<p>Ihr Termin am <b>5. M\xc3\xa4rz</b></p>",
    );
    let text = text_of(&eml);
    assert!(text.starts_with("Subject: Terminbestätigung\nFrom: Praxis <praxis@example.com>"), "{text}");
    assert!(text.contains("Ihr Termin am 5. März"));
    let mht = d.file(
        "Seite.mht",
        b"Subject: Rezept Apfelkuchen\r\nContent-Type: multipart/related; boundary=X\r\n\r\n\
          --X\r\nContent-Type: text/html\r\n\r\n<h1>Apfelkuchen</h1><p>Zimt nicht vergessen</p>\r\n\
          --X\r\nContent-Type: image/png\r\nContent-Location: bild.png\r\n\r\nPNG\r\n--X--\r\n",
    );
    let text = text_of(&mht);
    assert!(text.contains("Rezept Apfelkuchen") && text.contains("Zimt nicht vergessen") && !text.contains("Attachments"));
}

#[test]
fn compressed_rtf_bodies_of_old_outlook_emails() {
    let plain: &[u8] = br"{\rtf1 Hallo}";
    let mut stored = ((plain.len() + 12) as u32).to_le_bytes().to_vec();
    stored.extend((plain.len() as u32).to_le_bytes());
    stored.extend(b"MELA");
    stored.extend([0u8; 4]);
    stored.extend(plain);
    assert_eq!(mail::decompress_rtf(&stored).unwrap(), plain);
    // compressed: a reference to "{\rtf1" in the starting dictionary, then " Hallo}" as is
    let mut body = vec![0b0000_0001u8];
    body.extend((6u16 - 2).to_be_bytes());
    body.extend(b" Hallo}");
    let mut packed = ((body.len() + 12) as u32).to_le_bytes().to_vec();
    packed.extend(13u32.to_le_bytes());
    packed.extend(b"LZFu");
    packed.extend([0u8; 4]);
    packed.extend(&body);
    assert_eq!(mail::decompress_rtf(&packed).unwrap(), plain);
}

// ---- what isn't searched ---------------------------------------------------------------------------

#[test]
fn what_isnt_searched_is_named_for_what_it_is() {
    let t = Types::new(&[], &Default::default());
    assert_eq!(t.skip_dir("data", false, false, "Android"), Some("program_folder")); // apps' own data in a phone's copy
    assert_eq!(t.skip_dir("media", false, false, "Android"), None); // what apps saved for you
    assert_eq!(t.skip_dir("data", false, false, "Projekte"), None);
    let wanted = Types::new(&[], &[("program_folders".to_string(), true)].into());
    assert_eq!(wanted.skip_dir("data", false, false, "Android"), None);
    assert_eq!(t.skip_file("Drache.kra~", false, false), Some("backup"));
    assert_eq!(t.skip_file("Brief.docx.bak", false, false), Some("backup"));
    let why: Vec<&str> = ["Setup.exe", "Urlaub.mp4", "Postfach.pst", "Plakat.eps"].iter().map(|n| t.decide(n, None).unwrap_err()).collect();
    assert_eq!(why, ["program", "media", "mailbox", "type"]); // (a name it doesn't know: its content decides, given a path)
    let off = Types::new(&["pictures".to_string(), "archives".to_string()], &Default::default());
    assert_eq!((off.decide("Foto.jpg", None), off.decide("Brief.pdf", None)), (Err("off"), Ok("doc")));
    assert_eq!((off.decide("Fotos.zip", None), t.decide("Fotos.zip", None)), (Err("off"), Err("archive")));
}

// ---- by content, whatever the name ----------------------------------------------------------------

#[test]
fn documents_under_other_names_are_read_by_what_they_hold() {
    let d = Dir::new();
    let copy = |from: &str, to: &str| {
        let p = d.0.join(to);
        std::fs::copy(fixture(from), &p).unwrap();
        p
    };
    for (from, to, words) in [
        ("budget.docx", "Bericht.xyz", "Quarterly budget review"),
        ("Haushalt.ods", "Haushalt", "Strom | 59,50"),
        ("u.ppsx", "Vortrag.abc", "Urlaubsplanung"),
        ("Brief.doc", "Brief.alt", "die Miete steigt ab Mai"),
        ("Rechnung.msg", "Rechnung.mail", "Ihre Rechnung für März"),
        ("lease.pdf", "Mietvertrag.dokument", "Rental agreement"),
    ] {
        let p = copy(from, to);
        let ex = read(&p).unwrap_or_else(|e| panic!("{to}: {e}"));
        assert_eq!(ex.kind, "doc", "{to}");
        assert!(text_of(&p).contains(words), "{to}: {}", text_of(&p));
    }
}

#[test]
fn pictures_under_other_names_are_seen_by_what_they_hold() {
    let d = Dir::new();
    let photo = d.file("IMG_0042.bild", jpeg(640, 480, [10, 140, 200]));
    let drawing = d.file(
        "plan.zeichnung",
        r##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="300" height="200"><rect width="300" height="200" fill="#f54e00"/></svg>"##,
    );
    let with_preview = d.0.join("modell.3mf"); // a file that carries a picture of itself
    zip_of(&with_preview, &[("3D/3dmodel.model", b"<model/>"), ("preview.png", &png(400, 300))]);
    for (p, size) in [(&photo, (640, 480)), (&drawing, (1024, 683)), (&with_preview, (400, 300))] {
        let ex = read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert_eq!(ex.kind, "image");
        let (w, h) = ex.chunks[0].image.as_ref().unwrap().dimensions();
        assert!(w == size.0 && h.abs_diff(size.1) <= 1, "{}: {w} x {h}", p.display());
    }
    let archive = d.0.join("sicherung.xyz"); // a ZIP that is neither a document nor carries a picture
    zip_of(&archive, &[("a.txt", b"x")]);
    assert_eq!(skipped(&archive), "binary");
}
