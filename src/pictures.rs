//! Pictures: decoding, and resizing exactly as the model's own image processor does (Pillow's
//! resize), so the model sees the pixels it was made for: bicubic with antialiasing, fixed-point
//! weights, horizontal pass first into an 8-bit image, then the vertical one.

use image::RgbImage;

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bicubic(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// Per output pixel: (first input pixel, how many), and fixed-point weights (ksize each).
struct Coeffs {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    kk: Vec<i32>,
}

fn precompute(in_size: usize, in0: f64, in1: f64, out_size: usize) -> Coeffs {
    const SUPPORT: f64 = 2.0;
    let scale = (in1 - in0) / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = SUPPORT * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut kk = vec![0f64; out_size * ksize];
    let mut bounds = vec![(0usize, 0usize); out_size];
    let ss = 1.0 / filterscale;
    for xx in 0..out_size {
        let center = in0 + (xx as f64 + 0.5) * scale;
        let xmin = ((center - support + 0.5) as i64).max(0);
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) - xmin;
        let xmax = xmax.max(0) as usize;
        let k = &mut kk[xx * ksize..(xx + 1) * ksize];
        let mut ww = 0.0;
        for x in 0..xmax {
            let w = bicubic((x as f64 + xmin as f64 - center + 0.5) * ss);
            k[x] = w;
            ww += w;
        }
        if ww != 0.0 {
            for v in k.iter_mut().take(xmax) {
                *v /= ww;
            }
        }
        bounds[xx] = (xmin as usize, xmax);
    }
    let one = (1u32 << PRECISION_BITS) as f64;
    let kk = kk
        .iter()
        .map(|&v| if v < 0.0 { (-0.5 + v * one) as i32 } else { (0.5 + v * one) as i32 })
        .collect();
    Coeffs { ksize, bounds, kk }
}

#[inline]
fn clip8(acc: i64) -> u8 {
    (acc >> PRECISION_BITS).clamp(0, 255) as u8
}

fn horizontal(src: &RgbImage, out_w: u32, c: &Coeffs) -> RgbImage {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut out = RgbImage::new(out_w, h as u32);
    let s = src.as_raw();
    let o = out.as_mut();
    let half = 1i64 << (PRECISION_BITS - 1);
    for y in 0..h {
        let row = &s[y * w * 3..(y + 1) * w * 3];
        for xx in 0..out_w as usize {
            let (xmin, n) = c.bounds[xx];
            let k = &c.kk[xx * c.ksize..xx * c.ksize + n];
            let (mut a, mut b, mut d) = (half, half, half);
            for (i, &kv) in k.iter().enumerate() {
                let p = (xmin + i) * 3;
                let kv = kv as i64;
                a += row[p] as i64 * kv;
                b += row[p + 1] as i64 * kv;
                d += row[p + 2] as i64 * kv;
            }
            let at = (y * out_w as usize + xx) * 3;
            o[at] = clip8(a);
            o[at + 1] = clip8(b);
            o[at + 2] = clip8(d);
        }
    }
    out
}

fn vertical(src: &RgbImage, out_h: u32, c: &Coeffs) -> RgbImage {
    let w = src.width() as usize;
    let mut out = RgbImage::new(w as u32, out_h);
    let s = src.as_raw();
    let o = out.as_mut();
    let half = 1i64 << (PRECISION_BITS - 1);
    for yy in 0..out_h as usize {
        let (ymin, n) = c.bounds[yy];
        let k = &c.kk[yy * c.ksize..yy * c.ksize + n];
        for xx in 0..w {
            let (mut a, mut b, mut d) = (half, half, half);
            for (i, &kv) in k.iter().enumerate() {
                let p = ((ymin + i) * w + xx) * 3;
                let kv = kv as i64;
                a += s[p] as i64 * kv;
                b += s[p + 1] as i64 * kv;
                d += s[p + 2] as i64 * kv;
            }
            let at = (yy * w + xx) * 3;
            o[at] = clip8(a);
            o[at + 1] = clip8(b);
            o[at + 2] = clip8(d);
        }
    }
    out
}

/// Pillow's Image.resize(size, BICUBIC) of the whole picture.
pub fn resize_bicubic(src: &RgbImage, w: u32, h: u32) -> RgbImage {
    resize_box(src, w, h, (0.0, 0.0, src.width() as f64, src.height() as f64))
}

/// Pillow's resize of a part of the picture (box: left, top, right, bottom, fractional).
pub fn resize_box(src: &RgbImage, w: u32, h: u32, b: (f64, f64, f64, f64)) -> RgbImage {
    let need_h = w != src.width() || b.0 != 0.0 || b.2 != w as f64;
    let need_v = h != src.height() || b.1 != 0.0 || b.3 != h as f64;
    let mut img = std::borrow::Cow::Borrowed(src);
    if need_h {
        let c = precompute(src.width() as usize, b.0, b.2, w as usize);
        img = std::borrow::Cow::Owned(horizontal(&img, w, &c));
    }
    if need_v {
        let c = precompute(src.height() as usize, b.1, b.3, h as usize);
        img = std::borrow::Cow::Owned(vertical(&img, h, &c));
    }
    img.into_owned()
}

/// Pillow's Image.reduce(factor): each output pixel the mean of a factor_x x factor_y block
/// (the last ones over what's left), rounded.
pub fn reduce(src: &RgbImage, fx: u32, fy: u32, b: (u32, u32, u32, u32)) -> RgbImage {
    let (bw, bh) = (b.2 - b.0, b.3 - b.1);
    let (ow, oh) = (bw.div_ceil(fx), bh.div_ceil(fy));
    let mut out = RgbImage::new(ow, oh);
    for oy in 0..oh {
        let y0 = b.1 + oy * fy;
        let y1 = (y0 + fy).min(b.3);
        for ox in 0..ow {
            let x0 = b.0 + ox * fx;
            let x1 = (x0 + fx).min(b.2);
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let mut sum = [0u64; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = src.get_pixel(x, y).0;
                    sum[0] += p[0] as u64;
                    sum[1] += p[1] as u64;
                    sum[2] += p[2] as u64;
                }
            }
            let px = [0, 1, 2].map(|i| ((sum[i] + n / 2) / n) as u8);
            out.put_pixel(ox, oy, image::Rgb(px));
        }
    }
    out
}

/// The size Pillow's thumbnail((max, max)) gives a picture (None: it fits already).
pub fn thumbnail_size(w: u32, h: u32, max: u32) -> Option<(u32, u32)> {
    let (x, y) = (max as f64, max as f64);
    if x >= w as f64 && y >= h as f64 {
        return None;
    }
    let aspect = w as f64 / h as f64;
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> f64 {
        let (lo, hi) = (number.floor(), number.ceil());
        let pick = if key(hi) < key(lo) { hi } else { lo };
        pick.max(1.0)
    };
    if x / y >= aspect {
        let nx = round_aspect(y * aspect, &|n| (aspect - n / y).abs());
        Some((nx as u32, max))
    } else {
        let ny = round_aspect(x / aspect, &|n| if n == 0.0 { 0.0 } else { (aspect - x / n).abs() });
        Some((max, ny as u32))
    }
}

/// Pillow's thumbnail((max, max)) with its defaults (BICUBIC, reducing_gap 2.0): a box average
/// first when the picture is more than twice as big, then the bicubic resize.
pub fn thumbnail(src: &RgbImage, max: u32) -> RgbImage {
    let Some((w, h)) = thumbnail_size(src.width(), src.height(), max) else {
        return src.clone();
    };
    if (w, h) == (src.width(), src.height()) {
        return src.clone();
    }
    const GAP: f64 = 2.0;
    let (sw, sh) = (src.width() as f64, src.height() as f64);
    let fx = ((sw / w as f64 / GAP) as u32).max(1);
    let fy = ((sh / h as f64 / GAP) as u32).max(1);
    if fx > 1 || fy > 1 {
        // (Pillow's "safe box" for the whole picture is the whole picture.)
        let reduced = reduce(src, fx, fy, (0, 0, src.width(), src.height()));
        return resize_box(&reduced, w, h, (0.0, 0.0, sw / fx as f64, sh / fy as f64));
    }
    resize_bicubic(src, w, h)
}

// ---- reading pictures ------------------------------------------------------------------------

use std::io::{BufRead, Cursor, Read, Seek};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};

use crate::config::{self, suffix};
use crate::extract::{ReadError, ReadResult};

const MAX_ALLOC: u64 = 1 << 30; // bigger pictures aren't decoded

fn image_error(e: image::ImageError) -> ReadError {
    match e {
        image::ImageError::IoError(io) => ReadError::Io(io),
        image::ImageError::Limits(_) => ReadError::Skip("too_large"),
        _ => ReadError::Damaged("image"),
    }
}

/// Transparent parts on white, rounded as Pillow pastes them.
pub fn flatten(img: DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.into_rgb8();
    }
    let rgba = img.into_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (o, p) in out.pixels_mut().zip(rgba.pixels()) {
        let a = p.0[3] as u32;
        for i in 0..3 {
            let t = 255 * (255 - a) + p.0[i] as u32 * a + 128;
            o.0[i] = ((t + (t >> 8)) >> 8) as u8;
        }
    }
    out
}

/// How much smaller Pillow decodes a JPEG when asked for at most `max_side` (its "draft" mode,
/// at twice the size wanted): 1, 2, 4 or 8.
fn draft_scale(w: u32, h: u32, max_side: u32) -> u32 {
    let want = (max_side * 2).max(1);
    let scale = (w / want).min(h / want);
    [8, 4, 2, 1].into_iter().find(|&s| scale >= s).unwrap_or(1)
}

fn open<R: BufRead + Seek>(reader: ImageReader<R>, max_side: u32, tiny_check: bool) -> ReadResult<DynamicImage> {
    let mut reader = reader.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let jpeg = reader.format() == Some(image::ImageFormat::Jpeg);
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let (w, h) = decoder.dimensions();
    if tiny_check && w.min(h) < config::MIN_IMAGE_SIDE {
        return Err(ReadError::Skip("tiny_image"));
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    if jpeg {
        let s = draft_scale(w, h, max_side);
        if s > 1 {
            img = DynamicImage::ImageRgb8(reduce(&flatten(img), s, s, (0, 0, w, h)));
        }
    }
    img.apply_orientation(orientation);
    Ok(img)
}

/// A picture from bytes (inside a document or a book), whatever its size.
pub fn decode(data: &[u8]) -> ReadResult<RgbImage> {
    let img = match libjpeg(data, config::MAX_IMAGE_SIDE, false) {
        Some(r) => r?,
        None => open(ImageReader::new(Cursor::new(data)), config::MAX_IMAGE_SIDE, false)?,
    };
    Ok(flatten(img))
}

/// A picture from bytes, as the model sees it.
fn from_bytes(data: &[u8], max_side: u32) -> ReadResult<DynamicImage> {
    match libjpeg(data, max_side, true) {
        Some(r) => r,
        None => open(ImageReader::new(Cursor::new(data)), max_side, true),
    }
}

/// The turn a photo's EXIF data asks for.
fn exif_orientation(data: &[u8]) -> Orientation {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(data))
        .ok()
        .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)))
        .and_then(|v| Orientation::from_exif(v as u8))
        .unwrap_or(Orientation::NoTransforms)
}

/// JPEG through libjpeg, decoded as the model's image processor (Pillow) decodes it, its "draft"
/// scaling for big photos included: the same pixels. None: one it leaves to the other decoders
/// (CMYK, or one libjpeg gives up on).
fn libjpeg(data: &[u8], max_side: u32, tiny_check: bool) -> Option<ReadResult<DynamicImage>> {
    if !data.starts_with(b"\xff\xd8\xff") {
        return None;
    }
    let decoded = std::panic::catch_unwind(|| -> Option<ReadResult<RgbImage>> {
        let mut d = mozjpeg::Decompress::new_mem(data).ok()?;
        let (w, h) = (d.width() as u32, d.height() as u32);
        if tiny_check && w.min(h) < config::MIN_IMAGE_SIDE {
            return Some(Err(ReadError::Skip("tiny_image")));
        }
        if matches!(d.color_space(), mozjpeg::ColorSpace::JCS_CMYK | mozjpeg::ColorSpace::JCS_YCCK) {
            return None;
        }
        if (w as u64) * (h as u64) * 3 > MAX_ALLOC {
            return Some(Err(ReadError::Skip("too_large")));
        }
        let s = draft_scale(w, h, max_side);
        if s > 1 {
            d.scale((8 / s) as u8);
        }
        let mut started = d.rgb().ok()?;
        let (ow, oh) = (started.width() as u32, started.height() as u32);
        let pixels: Vec<u8> = started.read_scanlines().ok()?;
        let _ = started.finish(); // (a cut-off photo still shows what's there)
        Some(Ok(RgbImage::from_raw(ow, oh, pixels)?))
    })
    .ok()
    .flatten()?;
    Some(decoded.map(|img| {
        let mut img = DynamicImage::ImageRgb8(img);
        img.apply_orientation(exif_orientation(data));
        img
    }))
}

/// How a picture file is decoded: by its first bytes, which its name doesn't always match.
#[derive(PartialEq)]
enum Format {
    Svg,
    Raw,
    Preview,
    Heif,
    Psd,
    Other,
}

const HEIF_BRANDS: [&[u8]; 8] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1"];

fn format_of(path: &Path, ext: &str) -> Format {
    if ext == ".svg" {
        return Format::Svg;
    }
    if config::RAW_EXTS.contains(ext) {
        return Format::Raw; // (they start like TIFF pictures, which only shows a small part)
    }
    let head = crate::extract::head(path, 4096).unwrap_or_default();
    if head.starts_with(b"PK\x03\x04") {
        Format::Preview
    } else if head.starts_with(b"8BPS") {
        Format::Psd
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && HEIF_BRANDS.contains(&&head[8..12]) {
        Format::Heif
    } else if crate::extract::looks_like_svg(&head) {
        Format::Svg
    } else {
        Format::Other
    }
}

/// An image file, as the model sees it: upright, on white, at most max_side pixels.
pub fn load_image(path: &Path, max_side: u32) -> ReadResult<RgbImage> {
    let format = format_of(path, &suffix(&path.to_string_lossy()));
    let img = match format {
        Format::Svg => return svg(path, max_side, false),
        Format::Raw => from_bytes(&raw_preview(path)?, max_side)?,
        Format::Preview => from_bytes(&preview_inside(path)?, max_side)?,
        Format::Heif if heif::available() => heif::load(path, config::MIN_IMAGE_SIDE)?,
        Format::Psd => psd_image(path)?,
        _ => {
            let jpeg = crate::extract::head(path, 3).is_some_and(|h| h == b"\xff\xd8\xff");
            let first = if jpeg { libjpeg(&std::fs::read(path)?, max_side, true) } else { None };
            match first {
                Some(r) => r?,
                None => match ImageReader::open(path).map_err(ReadError::Io).and_then(|r| open(r, max_side, true)) {
                    Err(ReadError::Damaged(k)) => wic::load(path).map_err(|_| ReadError::Damaged(k))?,
                    other => other?,
                },
            }
        }
    };
    if img.width().min(img.height()) < config::MIN_IMAGE_SIDE && format == Format::Psd {
        return Err(ReadError::Skip("tiny_image"));
    }
    Ok(thumbnail(&flatten(img), max_side))
}

/// A file as a picture, for previews: an image itself, a document's first page, or the preview
/// picture a document carries.
pub fn picture(path: &Path, size: u32) -> ReadResult<RgbImage> {
    let ext = suffix(&path.to_string_lossy());
    if ext == ".svg" {
        return svg(path, size, true);
    }
    if crate::paged::has_pages(path) {
        return crate::paged::first_page(path, size);
    }
    if crate::extract::THUMBNAIL_EXTS.contains(&ext.as_str()) {
        let data = crate::documents::odf_thumbnail(path).ok_or(ReadError::Skip("nothing_to_show"))?;
        return decode(&data);
    }
    load_image(path, size)
}

/// The pictures a ZIP-based file may carry of itself: a layered painting's finished picture (Krita,
/// OpenRaster), or the preview many apps save inside their files.
pub const PREVIEWS: [&str; 3] = ["mergedimage.png", "preview.png", "Thumbnails/thumbnail.png"];

/// The picture a ZIP-based file carries of itself (see PREVIEWS).
fn preview_inside(path: &Path) -> ReadResult<Vec<u8>> {
    let mut z = match zip::ZipArchive::new(std::fs::File::open(path)?) {
        Ok(z) => z,
        Err(_) => return Err(ReadError::Damaged("image")),
    };
    for name in PREVIEWS {
        if let Ok(f) = z.by_name(name) {
            let mut out = Vec::new();
            f.take(config::MAX_RAW_BYTES).read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    Err(ReadError::Damaged("image"))
}

/// A camera raw file's biggest preview picture: every camera stores a JPEG inside.
pub fn raw_preview(path: &Path) -> ReadResult<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    // SAFETY: read only; a file changed meanwhile only gives a broken picture.
    let m = unsafe { memmap2::Mmap::map(&f)? };
    let finder = memchr::memmem::Finder::new(b"\xff\xd8\xff");
    let mut found = Vec::new();
    let mut at = finder.find(&m);
    while let Some(a) = at {
        let end = jpeg_end(&m, a);
        if end > 0 {
            found.push((end - a, a, end));
        }
        let from = if end > 0 { end } else { a + 3 };
        at = if from < m.len() { finder.find(&m[from..]).map(|i| i + from) } else { None };
    }
    found.sort_by(|x, y| y.cmp(x));
    for (_, start, end) in found {
        let data = &m[start..end];
        let size = ImageReader::new(Cursor::new(data)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        if let Some((w, h)) = size {
            if w.min(h) >= config::MIN_IMAGE_SIDE {
                return Ok(data.to_vec());
            }
        }
    }
    Err(ReadError::Damaged("image"))
}

/// Where the JPEG at `start` ends (0: it isn't one, or it's the camera's raw data stored as
/// lossless JPEG, which isn't a picture to look at).
fn jpeg_end(m: &[u8], start: usize) -> usize {
    let n = m.len();
    let mut pos = start + 2;
    while pos + 4 <= n {
        if m[pos] != 0xFF {
            return 0;
        }
        let marker = m[pos + 1];
        if marker == 0xFF {
            pos += 1; // fill byte
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        if matches!(marker, 0xC3 | 0xC7 | 0xCB | 0xCF) {
            return 0; // lossless
        }
        let length = (m[pos + 2] as usize) << 8 | m[pos + 3] as usize;
        if length < 2 {
            return 0;
        }
        pos += 2 + length;
        if marker != 0xDA {
            continue; // not yet the image data
        }
        loop {
            // the image data runs to the next marker
            if pos >= n {
                return 0;
            }
            match memchr::memchr(0xFF, &m[pos..]) {
                None => return 0,
                Some(i) => pos += i,
            }
            if pos + 1 >= n {
                return 0;
            }
            let following = m[pos + 1];
            if following == 0x00 || (0xD0..=0xD7).contains(&following) {
                pos += 2;
            } else if following == 0xFF {
                pos += 1;
            } else if following == 0xD9 {
                return pos + 2;
            } else {
                break; // another segment (progressive JPEGs have several scans)
            }
        }
    }
    0
}

fn psd_image(path: &Path) -> ReadResult<DynamicImage> {
    let data = std::fs::read(path)?;
    let psd = psd::Psd::from_bytes(&data).map_err(|_| ReadError::Damaged("image"))?;
    let rgba = image::RgbaImage::from_raw(psd.width(), psd.height(), psd.rgba()).ok_or(ReadError::Damaged("image"))?;
    Ok(DynamicImage::ImageRgba8(rgba))
}

/// A book's first page as a reader lays it out (400 x 600 points, 12 point text), `size` pixels
/// on its longer side: for previews of books without pictures.
pub fn text_page(text: &str, size: u32) -> ReadResult<RgbImage> {
    const W: f64 = 400.0;
    const H: f64 = 600.0;
    const MARGIN: f64 = 24.0;
    const FONT: f64 = 12.0;
    let per_line = ((W - 2.0 * MARGIN) / (FONT * 0.5)) as usize;
    let mut lines: Vec<String> = Vec::new();
    'all: for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > per_line {
                lines.push(std::mem::take(&mut line));
                if lines.len() as f64 * FONT * 1.2 > H - 2.0 * MARGIN {
                    break 'all;
                }
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
        if lines.len() as f64 * FONT * 1.2 > H - 2.0 * MARGIN {
            break;
        }
    }
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}"><rect width="{W}" height="{H}" fill="#fff"/><text font-family="Georgia, 'Times New Roman', serif" font-size="{FONT}" fill="#111">"##
    );
    for (i, l) in lines.iter().enumerate() {
        let y = MARGIN + FONT + i as f64 * FONT * 1.2;
        if y > H - MARGIN {
            break;
        }
        svg += &format!(r#"<tspan x="{MARGIN}" y="{y:.1}">{}</tspan>"#, esc(l));
    }
    svg += "</text></svg>";
    let options = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_str(&svg, &options).map_err(|_| ReadError::Damaged("document"))?;
    let zoom = size as f64 / H;
    let (pw, ph) = (crate::paged::pixels(W * zoom), crate::paged::pixels(H * zoom));
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or(ReadError::Skip("too_large"))?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(zoom as f32, zoom as f32), &mut pixmap.as_mut());
    let rgb: Vec<u8> = pixmap.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    RgbImage::from_raw(pw, ph, rgb).ok_or(ReadError::Damaged("document"))
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// A drawing (SVG) on white. For the model: icons are skipped, and it's drawn at most 4x its size;
/// for a preview, at `size` pixels on its longer side.
fn svg(path: &Path, size: u32, preview: bool) -> ReadResult<RgbImage> {
    use resvg::tiny_skia;
    let data = std::fs::read(path)?;
    let options = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_data(&data, &options).map_err(|_| ReadError::Damaged("image"))?;
    let (w, h) = (tree.size().width(), tree.size().height());
    let zoom = if preview {
        size as f32 / w.max(h).max(1.0)
    } else {
        if w.min(h) < config::MIN_IMAGE_SIDE as f32 {
            return Err(ReadError::Skip("tiny_image")); // icons
        }
        (size as f32 / w.max(h)).min(4.0)
    };
    let (pw, ph) = (crate::paged::pixels(w as f64 * zoom as f64), crate::paged::pixels(h as f64 * zoom as f64));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or(ReadError::Skip("too_large"))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(&tree, tiny_skia::Transform::from_scale(zoom, zoom), &mut pixmap.as_mut());
    let rgb: Vec<u8> = pixmap.data().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    RgbImage::from_raw(pw, ph, rgb).ok_or(ReadError::Damaged("image"))
}

/// Windows' own decoders (WIC): HEIC, AVIF and the TIFF and JPEG variants the image library
/// doesn't read, with the codecs installed on this PC.
mod wic {
    use std::path::Path;

    use image::DynamicImage;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;
    use windows::core::HSTRING;

    use crate::extract::{ReadError, ReadResult};

    pub fn load(path: &Path) -> ReadResult<DynamicImage> {
        let decoded = unsafe {
            (|| -> windows::core::Result<(u32, u32, Vec<u8>)> {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                let decoder = factory.CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )?;
                let frame = decoder.GetFrame(0)?;
                let (mut w, mut h) = (0u32, 0u32);
                frame.GetSize(&mut w, &mut h)?;
                if w.min(h) < crate::config::MIN_IMAGE_SIDE || (w as u64 * h as u64 * 4) > super::MAX_ALLOC {
                    return Ok((w, h, Vec::new()));
                }
                let source = WICConvertBitmapSource(&GUID_WICPixelFormat32bppRGBA, &frame)?;
                let mut buf = vec![0u8; w as usize * h as usize * 4];
                source.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
                Ok((w, h, buf))
            })()
        };
        let (w, h, buf) = decoded.map_err(|_| ReadError::Damaged("image"))?;
        if w.min(h) < crate::config::MIN_IMAGE_SIDE {
            return Err(ReadError::Skip("tiny_image"));
        }
        if buf.is_empty() {
            return Err(ReadError::Skip("too_large"));
        }
        let rgba = image::RgbaImage::from_raw(w, h, buf).ok_or(ReadError::Damaged("image"))?;
        Ok(DynamicImage::ImageRgba8(rgba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_under_transparency_and_draft() {
        let mut rgba = image::RgbaImage::new(1, 1);
        rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 128]));
        let out = flatten(DynamicImage::ImageRgba8(rgba));
        assert_eq!(out.get_pixel(0, 0).0, [127, 127, 127]);
        assert_eq!(draft_scale(6000, 4000, 1024), 1);
        assert_eq!(draft_scale(8192, 6144, 1024), 2);
    }

    #[test]
    fn thumbnail_sizes_like_pillow() {
        assert_eq!(thumbnail_size(4000, 3000, 1024), Some((1024, 768)));
        assert_eq!(thumbnail_size(100, 50, 1024), None);
    }

    #[test]
    fn jpeg_end_finds_the_end() {
        // SOI, an APP0 segment of 4 bytes, SOS of 2 bytes, two bytes of data, EOI.
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 1, 2, 0xFF, 0xDA, 0x00, 0x02, 7, 8, 0xFF, 0xD9];
        assert_eq!(jpeg_end(&jpeg, 0), jpeg.len());
    }
}

/// HEIC photos (iPhones and others) through libheif (in vendor\libheif, when it's there).
mod heif {
    use std::ffi::{c_char, c_int, c_void};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use image::DynamicImage;

    use crate::extract::{ReadError, ReadResult};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct HeifError {
        code: c_int,
        subcode: c_int,
        message: *const c_char,
    }

    const COLORSPACE_RGB: c_int = 1;
    const CHROMA_RGB: c_int = 10;
    const CHROMA_RGBA: c_int = 11;
    const CHANNEL_INTERLEAVED: c_int = 10;

    type Ctx = *mut c_void;
    type Handle = *mut c_void;
    type Img = *mut c_void;

    struct Lib {
        _lib: libloading::os::windows::Library,
        alloc: unsafe extern "C" fn() -> Ctx,
        free: unsafe extern "C" fn(Ctx),
        read: unsafe extern "C" fn(Ctx, *const c_void, usize, *const c_void) -> HeifError,
        primary: unsafe extern "C" fn(Ctx, *mut Handle) -> HeifError,
        has_alpha: unsafe extern "C" fn(Handle) -> c_int,
        decode: unsafe extern "C" fn(Handle, *mut Img, c_int, c_int, *const c_void) -> HeifError,
        plane: unsafe extern "C" fn(Img, c_int, *mut c_int) -> *const u8,
        width: unsafe extern "C" fn(Img, c_int) -> c_int,
        height: unsafe extern "C" fn(Img, c_int) -> c_int,
        release_image: unsafe extern "C" fn(Img),
        release_handle: unsafe extern "C" fn(Handle),
    }

    // (function pointers and a loaded library: safe to share)
    unsafe impl Send for Lib {}
    unsafe impl Sync for Lib {}

    /// libheif in the app's folder, or in vendor/libheif while developing.
    fn find() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let mut dir = exe.parent()?.to_path_buf();
        for _ in 0..4 {
            for d in [dir.clone(), dir.join("vendor").join("libheif")] {
                if let Ok(entries) = std::fs::read_dir(&d) {
                    for e in entries.flatten() {
                        let name = e.file_name().to_string_lossy().to_lowercase();
                        if name.starts_with("libheif") && name.ends_with(".dll") {
                            return Some(e.path());
                        }
                    }
                }
            }
            dir = dir.parent()?.to_path_buf();
        }
        None
    }

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| {
            let path = find()?;
            unsafe {
                // Its own folder first, for the libraries it needs.
                let lib = libloading::os::windows::Library::load_with_flags(
                    &path,
                    libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
                )
                .ok()?;
                if let Ok(init) = lib.get::<unsafe extern "C" fn(*mut c_void) -> HeifError>(b"heif_init\0") {
                    init(std::ptr::null_mut());
                }
                macro_rules! f {
                    ($name:literal) => {
                        *lib.get($name).ok()?
                    };
                }
                Some(Lib {
                    alloc: f!(b"heif_context_alloc\0"),
                    free: f!(b"heif_context_free\0"),
                    read: f!(b"heif_context_read_from_memory_without_copy\0"),
                    primary: f!(b"heif_context_get_primary_image_handle\0"),
                    has_alpha: f!(b"heif_image_handle_has_alpha_channel\0"),
                    decode: f!(b"heif_decode_image\0"),
                    plane: f!(b"heif_image_get_plane_readonly\0"),
                    width: f!(b"heif_image_get_width\0"),
                    height: f!(b"heif_image_get_height\0"),
                    release_image: f!(b"heif_image_release\0"),
                    release_handle: f!(b"heif_image_handle_release\0"),
                    _lib: lib,
                })
            }
        })
        .as_ref()
    }

    pub fn available() -> bool {
        lib().is_some()
    }

    /// The photo, upright (libheif applies its turns), or why not.
    pub fn load(path: &Path, min_side: u32) -> ReadResult<DynamicImage> {
        let l = lib().ok_or(ReadError::Damaged("image"))?;
        let data = std::fs::read(path)?;
        let bad = || ReadError::Damaged("image");
        unsafe {
            let ctx = (l.alloc)();
            if ctx.is_null() {
                return Err(bad());
            }
            let result = (|| -> ReadResult<DynamicImage> {
                if (l.read)(ctx, data.as_ptr() as *const c_void, data.len(), std::ptr::null()).code != 0 {
                    return Err(bad());
                }
                let mut handle: Handle = std::ptr::null_mut();
                if (l.primary)(ctx, &mut handle).code != 0 || handle.is_null() {
                    return Err(bad());
                }
                let alpha = (l.has_alpha)(handle) != 0;
                let mut img: Img = std::ptr::null_mut();
                let chroma = if alpha { CHROMA_RGBA } else { CHROMA_RGB };
                let decoded = (l.decode)(handle, &mut img, COLORSPACE_RGB, chroma, std::ptr::null());
                (l.release_handle)(handle);
                if decoded.code != 0 || img.is_null() {
                    return Err(bad());
                }
                let (w, h) = ((l.width)(img, CHANNEL_INTERLEAVED), (l.height)(img, CHANNEL_INTERLEAVED));
                let mut stride: c_int = 0;
                let plane = (l.plane)(img, CHANNEL_INTERLEAVED, &mut stride);
                let channels = if alpha { 4 } else { 3 };
                let out = if plane.is_null() || w <= 0 || h <= 0 || stride < w * channels {
                    Err(bad())
                } else if (w.min(h) as u32) < min_side {
                    Err(ReadError::Skip("tiny_image"))
                } else {
                    let mut pixels = Vec::with_capacity((w * h * channels) as usize);
                    for y in 0..h {
                        let row = std::slice::from_raw_parts(plane.add((y * stride) as usize), (w * channels) as usize);
                        pixels.extend_from_slice(row);
                    }
                    if alpha {
                        image::RgbaImage::from_raw(w as u32, h as u32, pixels).map(DynamicImage::ImageRgba8).ok_or_else(bad)
                    } else {
                        image::RgbImage::from_raw(w as u32, h as u32, pixels).map(DynamicImage::ImageRgb8).ok_or_else(bad)
                    }
                };
                (l.release_image)(img);
                out
            })();
            (l.free)(ctx);
            result
        }
    }
}
