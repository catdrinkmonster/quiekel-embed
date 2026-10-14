//! Files inside other files, searched like files in a folder, without unpacking anything:
//! archives (ZIP, 7z, RAR, TAR, CAB, ISO, ...), single compressed files (.gz) and email attachments.
//!
//! A file inside gets a path through its container, as if the container were a folder:
//! C:\Docs\photos.zip\2019\beach.jpg, or C:\Mail\offer.msg\prices.pdf. A container inside a
//! container is looked into too, once (an email in a ZIP, a ZIP in a ZIP); deeper ones are left
//! alone, and so are archives with very many files (mostly backups). Whatever is left out comes
//! with its reason, for the folder's details.
//!
//! Indexing reads one entry at a time, with limits (an archive can lie about its sizes), and
//! opening a result copies just that file to a temp folder. Nothing is ever written next to your
//! files. 7z, RAR, CAB, ISO and LZH are read by Windows' own archive library, the one File
//! Explorer uses (Windows 11, 2023 and later); ZIP, TAR and .gz by the app itself.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::UNIX_EPOCH;

use anyhow::{Result, anyhow};
use parking_lot::Mutex;
use regex::Regex;

use crate::config::{file_name, suffix};

pub const MAX_ENTRIES: usize = 20_000; // bigger archives are mostly backups (unless Settings say otherwise)
pub const MAX_ENTRIES_BIG: usize = 1_000_000;
pub const MAX_NESTED_BYTES: u64 = 256 * 1024 * 1024; // a container inside a container is read into memory, up to this
const MAX_OPEN: usize = 4;

const ZIP_EXTS: [&str; 1] = [".zip"];
const TAR_EXTS: [&str; 5] = [".tar", ".tgz", ".tbz", ".tbz2", ".txz"];
const SINGLE_EXTS: [&str; 3] = [".gz", ".bz2", ".xz"]; // one compressed file (unless it's a .tar.gz)
const LIBRARY_EXTS: [&str; 9] = [".7z", ".rar", ".cab", ".iso", ".lzh", ".lha", ".zipx", ".xar", ".tzst"];
const MAIL_EXTS: [&str; 2] = [".eml", ".msg"];

static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.part\d+\.rar$").unwrap()); // one part of a split archive

/// Why an entry can't be read.
#[derive(Debug)]
pub enum ArchiveError {
    TooLarge,
    /// Behind a password.
    Encrypted,
    /// This Windows can't open the archive type (its archive library came with Windows 11, 2023).
    NeedsWindows,
    /// Damaged, or not what its name says.
    Broken(String),
    NotFound,
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::TooLarge => write!(f, "too large"),
            ArchiveError::Encrypted => write!(f, "encrypted"),
            ArchiveError::NeedsWindows => write!(f, "needs a newer Windows"),
            ArchiveError::Broken(m) => write!(f, "unreadable archive: {m}"),
            ArchiveError::NotFound => write!(f, "not in the archive"),
        }
    }
}
impl std::error::Error for ArchiveError {}

fn broken(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ArchiveError::Broken(message.into()))
}

/// "zip", "tar", "single", "library" (Windows' archive library) or "mail"; None for plain files.
pub fn container_type(name: &str) -> Option<&'static str> {
    let lower = file_name(name).to_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tar.bz2") || lower.ends_with(".tar.xz") {
        return Some("tar");
    }
    if lower.ends_with(".tar.zst") {
        return Some("library");
    }
    let ext = suffix(&lower);
    let e = ext.as_str();
    if ZIP_EXTS.contains(&e) {
        Some("zip")
    } else if TAR_EXTS.contains(&e) {
        Some("tar")
    } else if SINGLE_EXTS.contains(&e) {
        Some("single")
    } else if LIBRARY_EXTS.contains(&e) {
        Some("library")
    } else if MAIL_EXTS.contains(&e) {
        Some("mail")
    } else {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String, // as stored
    pub size: u64,
    pub mtime: f64, // 0: unknown, the container's own time is used
    pub encrypted: bool,
    pub inline: bool, // an email's picture shown inside its text
}

impl Entry {
    fn new(name: String, size: u64, mtime: f64) -> Entry {
        Entry { name, size, mtime, encrypted: false, inline: false }
    }
}

/// Something found inside a container: a file, or why something there can't be searched.
#[derive(Clone, Debug)]
pub struct Item {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: f64,
    pub problem: Option<&'static str>,
}

/// An entry's name as folders and file name; None for odd or malicious names.
pub fn parts_of(name: &str) -> Option<Vec<String>> {
    let parts: Vec<String> =
        name.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect();
    if parts.is_empty() || parts.iter().any(|p| p == ".." || p.contains(':')) {
        return None;
    }
    Some(parts)
}

// ---- where a container's bytes come from ------------------------------------------------

#[derive(Clone)]
enum Source {
    Path(PathBuf),
    Bytes(Arc<Vec<u8>>),
}

#[derive(Clone)]
struct Shared(Arc<Vec<u8>>);
impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

impl Source {
    fn open(&self) -> io::Result<Box<dyn ReadSeek>> {
        Ok(match self {
            Source::Path(p) => Box::new(BufReader::with_capacity(1 << 16, File::open(p)?)),
            Source::Bytes(b) => Box::new(Cursor::new(Shared(b.clone()))),
        })
    }

    fn bytes(&self) -> io::Result<Arc<Vec<u8>>> {
        match self {
            Source::Path(p) => Ok(Arc::new(std::fs::read(p)?)),
            Source::Bytes(b) => Ok(b.clone()),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Method {
    Plain,
    Gzip,
    Bzip2,
    Xz,
}

fn method_of(head: &[u8]) -> Method {
    if head.starts_with(b"\x1f\x8b") {
        Method::Gzip
    } else if head.starts_with(b"BZh") {
        Method::Bzip2
    } else if head.starts_with(b"\xfd7zXZ\x00") {
        Method::Xz
    } else {
        Method::Plain
    }
}

fn decoder(method: Method, raw: Box<dyn ReadSeek>) -> Box<dyn Read + Send> {
    match method {
        Method::Plain => raw,
        Method::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        Method::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(raw)),
        Method::Xz => Box::new(XzReader::new(raw)),
    }
}

/// .xz unpacked on a thread of its own, handed over in pieces: a reader like the others.
struct XzReader {
    rx: crossbeam_channel::Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    at: usize,
}

struct ChunkWriter {
    tx: crossbeam_channel::Sender<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
}

impl Write for ChunkWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= 1 << 16 {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let piece = std::mem::take(&mut self.buf);
            self.tx.send(Ok(piece)).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(())
    }
}

impl XzReader {
    fn new(raw: Box<dyn ReadSeek>) -> XzReader {
        let (tx, rx) = crossbeam_channel::bounded(4);
        std::thread::Builder::new()
            .name("xz".into())
            .spawn(move || {
                let mut input = BufReader::new(raw);
                let mut out = ChunkWriter { tx: tx.clone(), buf: Vec::with_capacity(1 << 16) };
                let done = lzma_rs::xz_decompress(&mut input, &mut out)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
                    .and_then(|_| out.flush());
                if let Err(e) = done {
                    let _ = tx.send(Err(e));
                }
            })
            .ok();
        XzReader { rx, buf: Vec::new(), at: 0 }
    }
}

impl Read for XzReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.at < self.buf.len() {
                let n = out.len().min(self.buf.len() - self.at);
                out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            match self.rx.recv() {
                Ok(Ok(piece)) => {
                    self.buf = piece;
                    self.at = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(0),
            }
        }
    }
}

/// A container's unpacked bytes, read front to back; going back starts over (or seeks, when
/// nothing needs unpacking).
struct Stream {
    src: Source,
    method: Method,
    plain: Option<Box<dyn ReadSeek>>,
    packed: Option<Box<dyn Read + Send>>,
    pos: u64,
}

impl Stream {
    fn open(src: Source) -> io::Result<Stream> {
        let mut raw = src.open()?;
        let mut head = [0u8; 6];
        let n = read_up_to(&mut raw, &mut head)?;
        raw.seek(SeekFrom::Start(0))?;
        let method = method_of(&head[..n]);
        let mut s = Stream { src, method, plain: None, packed: None, pos: 0 };
        if method == Method::Plain {
            s.plain = Some(raw);
        } else {
            s.packed = Some(decoder(method, raw));
        }
        Ok(s)
    }

    fn go_to(&mut self, offset: u64) -> io::Result<()> {
        if let Some(p) = self.plain.as_mut() {
            p.seek(SeekFrom::Start(offset))?;
            self.pos = offset;
            return Ok(());
        }
        if offset < self.pos || self.packed.is_none() {
            self.packed = Some(decoder(self.method, self.src.open()?));
            self.pos = 0;
        }
        let skip = offset - self.pos;
        let copied = io::copy(&mut (&mut *self).take(skip), &mut io::sink())?;
        if copied < skip {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = match (self.plain.as_mut(), self.packed.as_mut()) {
            (Some(p), _) => p.read(out)?,
            (None, Some(p)) => p.read(out)?,
            (None, None) => 0,
        };
        self.pos += n as u64;
        Ok(n)
    }
}

fn read_up_to(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// At most limit + 1 bytes: one more than allowed tells it's too big.
fn read_limited(r: &mut dyn Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    r.take(limit.saturating_add(1)).read_to_end(&mut out)?;
    Ok(out)
}

// ---- containers ------------------------------------------------------------------------

trait Container: Send {
    fn entries(&mut self) -> Result<Vec<Entry>>;
    /// The bytes of entries()[index]: at most limit + 1.
    fn read_entry(&mut self, index: usize, limit: u64) -> Result<Vec<u8>>;
}

/// A container while it's open: what's in it, found by cleaned-up names.
struct Open {
    inner: Box<dyn Container>,
    entries: Option<Arc<Vec<Entry>>>,
    clean: Option<HashMap<String, usize>>,
}

impl Open {
    fn new(inner: Box<dyn Container>) -> Open {
        Open { inner, entries: None, clean: None }
    }

    fn entries(&mut self) -> Result<Arc<Vec<Entry>>> {
        if self.entries.is_none() {
            self.entries = Some(Arc::new(self.inner.entries()?));
        }
        Ok(self.entries.clone().unwrap())
    }

    /// The entry behind a cleaned-up name ("a/b.txt", as in the paths we hand out).
    fn find(&mut self, clean: &str) -> Result<Option<usize>> {
        if self.clean.is_none() {
            let mut map = HashMap::new();
            for (i, e) in self.entries()?.iter().enumerate() {
                if let Some(parts) = parts_of(&e.name) {
                    map.entry(parts.join("/").to_lowercase()).or_insert(i);
                }
            }
            self.clean = Some(map);
        }
        Ok(self.clean.as_ref().unwrap().get(&clean.to_lowercase()).copied())
    }

    fn read(&mut self, clean: &str, limit: u64) -> Result<Vec<u8>> {
        let index = self.find(clean)?.ok_or(ArchiveError::NotFound)?;
        let entry = self.entries()?[index].clone();
        if entry.encrypted {
            return Err(ArchiveError::Encrypted.into());
        }
        if entry.size > limit {
            return Err(ArchiveError::TooLarge.into());
        }
        let data = self.inner.read_entry(index, limit)?;
        if data.len() as u64 > limit {
            return Err(ArchiveError::TooLarge.into());
        }
        Ok(data)
    }
}

/// Windows' own ZIP folders store names in the PC's old "OEM" code page (850 for most of
/// Western Europe), unless they say they're UTF-8.
fn oem_decode(raw: &[u8]) -> String {
    use windows::Win32::Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    unsafe {
        let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, None);
        if n <= 0 {
            return String::from_utf8_lossy(raw).into_owned();
        }
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, Some(&mut wide));
        wide.truncate(n.max(0) as usize);
        String::from_utf16_lossy(&wide)
    }
}

fn local_timestamp(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> f64 {
    use chrono::{Local, TimeZone};
    Local.with_ymd_and_hms(y, mo, d, h, mi, s).earliest().map(|t| t.timestamp() as f64).unwrap_or(0.0)
}

struct Zip<R: Read + Seek + Send> {
    z: zip::ZipArchive<R>,
    index: Vec<usize>, // entries()[i] is the archive's file index[i]
}

impl<R: Read + Seek + Send> Zip<R> {
    fn new(reader: R) -> Result<Self> {
        let z = zip::ZipArchive::new(reader).map_err(|e| broken(e.to_string()))?;
        Ok(Zip { z, index: Vec::new() })
    }
}

impl<R: Read + Seek + Send> Container for Zip<R> {
    fn entries(&mut self) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        self.index.clear();
        for i in 0..self.z.len() {
            let f = self.z.by_index_raw(i).map_err(|e| broken(e.to_string()))?;
            let raw = f.name_raw();
            let mut name = if raw.is_ascii() || std::str::from_utf8(raw).is_ok_and(|s| s == f.name()) {
                f.name().to_string()
            } else {
                oem_decode(raw)
            };
            if let Some(nul) = name.find('\0') {
                name.truncate(nul);
            }
            let name = name.replace('\\', "/");
            if name.ends_with('/') {
                continue;
            }
            let mtime = f
                .last_modified()
                .map(|d| {
                    local_timestamp(d.year() as i32, d.month() as u32, d.day() as u32, d.hour() as u32, d.minute() as u32, d.second() as u32)
                })
                .unwrap_or(0.0);
            let mut e = Entry::new(name, f.size(), mtime);
            e.encrypted = f.encrypted();
            out.push(e);
            self.index.push(i);
        }
        Ok(out)
    }

    fn read_entry(&mut self, index: usize, limit: u64) -> Result<Vec<u8>> {
        let i = *self.index.get(index).ok_or(ArchiveError::NotFound)?;
        let mut f = self.z.by_index(i).map_err(|e| match e {
            zip::result::ZipError::UnsupportedArchive(m) if m.to_lowercase().contains("password") => {
                anyhow::Error::new(ArchiveError::Encrypted)
            }
            e => broken(e.to_string()),
        })?;
        Ok(read_limited(&mut f, limit).map_err(|e| broken(e.to_string()))?)
    }
}

struct Tar {
    stream: Stream,
    offsets: Vec<u64>, // where each entry's bytes start, unpacked
    listed: Option<Vec<Entry>>,
}

impl Tar {
    fn new(src: Source) -> Result<Tar> {
        let mut t = Tar { stream: Stream::open(src)?, offsets: Vec::new(), listed: None };
        t.list()?;
        Ok(t)
    }

    /// The regular files, read once (unpacking a compressed TAR takes reading all of it).
    fn list(&mut self) -> Result<()> {
        self.stream.go_to(0)?;
        let mut out: Vec<Entry> = Vec::new();
        let mut offsets = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut archive = tar::Archive::new(&mut self.stream);
        let mut first = true;
        let entries = archive.entries().map_err(|e| broken(e.to_string()))?;
        for item in entries {
            let e = match item {
                Ok(e) => e,
                Err(e) if first => return Err(broken(e.to_string())),
                Err(_) => break, // what's after a damaged header is lost, what's before isn't
            };
            first = false;
            let kind = e.header().entry_type();
            if !(kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse()) {
                continue;
            }
            let name = String::from_utf8_lossy(&e.path_bytes()).into_owned();
            let entry = Entry::new(name.clone(), e.size(), e.header().mtime().unwrap_or(0) as f64);
            let offset = e.raw_file_position();
            // The same name twice: the last one counts, in the first one's place.
            match by_name.get(&name) {
                Some(&i) => {
                    out[i] = entry;
                    offsets[i] = offset;
                }
                None => {
                    by_name.insert(name, out.len());
                    out.push(entry);
                    offsets.push(offset);
                }
            }
        }
        if first && self.stream.pos == 0 {
            return Err(broken("empty file"));
        }
        self.offsets = offsets;
        self.listed = Some(out);
        Ok(())
    }
}

impl Container for Tar {
    fn entries(&mut self) -> Result<Vec<Entry>> {
        Ok(self.listed.clone().unwrap_or_default())
    }

    fn read_entry(&mut self, index: usize, limit: u64) -> Result<Vec<u8>> {
        let offset = *self.offsets.get(index).ok_or(ArchiveError::NotFound)?;
        let size = self.listed.as_ref().map(|l| l[index].size).unwrap_or(0);
        self.stream.go_to(offset).map_err(|e| broken(e.to_string()))?;
        let mut out = Vec::new();
        (&mut self.stream).take(size.min(limit.saturating_add(1))).read_to_end(&mut out)?;
        Ok(out)
    }
}

/// A single compressed file: notes.txt.gz holds notes.txt.
struct Single {
    src: Source,
    method: Method,
    name: String,
    size: u64,
}

impl Single {
    fn new(src: Source, label: &str) -> Result<Single> {
        let ext = suffix(label);
        let method = match ext.as_str() {
            ".gz" => Method::Gzip,
            ".bz2" => Method::Bzip2,
            _ => Method::Xz,
        };
        let base = file_name(label);
        let name = base[..base.len() - ext.len()].to_string();
        let name = if name.is_empty() { "file".to_string() } else { name };
        let mut s = Single { src, method, name, size: 0 };
        // How big it is unpacked: count (up to a limit).
        let mut r = s.open()?;
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = r.read(&mut buf).map_err(|e| broken(e.to_string()))?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > MAX_NESTED_BYTES {
                break;
            }
        }
        s.size = size;
        Ok(s)
    }

    fn open(&self) -> Result<Box<dyn Read + Send>> {
        Ok(decoder(self.method, self.src.open()?))
    }
}

impl Container for Single {
    fn entries(&mut self) -> Result<Vec<Entry>> {
        Ok(vec![Entry::new(self.name.clone(), self.size, 0.0)])
    }

    fn read_entry(&mut self, _index: usize, limit: u64) -> Result<Vec<u8>> {
        let mut r = self.open()?;
        Ok(read_limited(&mut r, limit).map_err(|e| broken(e.to_string()))?)
    }
}

/// An email's attachments.
struct MailBox {
    mail: crate::mail::Mail,
}

impl Container for MailBox {
    fn entries(&mut self) -> Result<Vec<Entry>> {
        Ok(self
            .mail
            .attachments
            .iter()
            .map(|a| Entry { name: a.name.clone(), size: a.size, mtime: 0.0, encrypted: false, inline: a.inline })
            .collect())
    }

    fn read_entry(&mut self, index: usize, _limit: u64) -> Result<Vec<u8>> {
        self.mail.attachments.get(index).ok_or(ArchiveError::NotFound)?.load()
    }
}

// Windows' archive library (libarchive), loaded from System32 by its full path: never a DLL of
// the same name from elsewhere.
const EOF: c_int = 1;
const WARN: c_int = -20;
const REGULAR: u16 = 0o100000;

type Ptr = *mut c_void;

struct Lib {
    _lib: libloading::Library,
    read_new: unsafe extern "C" fn() -> Ptr,
    support_filter_all: unsafe extern "C" fn(Ptr) -> c_int,
    support_format_all: unsafe extern "C" fn(Ptr) -> c_int,
    open_filename_w: unsafe extern "C" fn(Ptr, *const u16, usize) -> c_int,
    open_memory: unsafe extern "C" fn(Ptr, *const c_void, usize) -> c_int,
    next_header: unsafe extern "C" fn(Ptr, *mut Ptr) -> c_int,
    read_data: unsafe extern "C" fn(Ptr, *mut c_void, usize) -> isize,
    free: unsafe extern "C" fn(Ptr) -> c_int,
    error_string: unsafe extern "C" fn(Ptr) -> *const c_char,
    pathname_w: unsafe extern "C" fn(Ptr) -> *const u16,
    pathname: unsafe extern "C" fn(Ptr) -> *const c_char,
    size: unsafe extern "C" fn(Ptr) -> i64,
    size_is_set: unsafe extern "C" fn(Ptr) -> c_int,
    mtime: unsafe extern "C" fn(Ptr) -> i64,
    filetype: unsafe extern "C" fn(Ptr) -> u16,
    is_encrypted: unsafe extern "C" fn(Ptr) -> c_int,
}

fn library() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = Path::new(&root).join("System32").join("archiveint.dll");
        unsafe {
            let lib = libloading::Library::new(&path).ok()?;
            macro_rules! f {
                ($name:literal) => {
                    *lib.get($name).ok()?
                };
            }
            Some(Lib {
                read_new: f!(b"archive_read_new\0"),
                support_filter_all: f!(b"archive_read_support_filter_all\0"),
                support_format_all: f!(b"archive_read_support_format_all\0"),
                open_filename_w: f!(b"archive_read_open_filename_w\0"),
                open_memory: f!(b"archive_read_open_memory\0"),
                next_header: f!(b"archive_read_next_header\0"),
                read_data: f!(b"archive_read_data\0"),
                free: f!(b"archive_read_free\0"),
                error_string: f!(b"archive_error_string\0"),
                pathname_w: f!(b"archive_entry_pathname_w\0"),
                pathname: f!(b"archive_entry_pathname\0"),
                size: f!(b"archive_entry_size\0"),
                size_is_set: f!(b"archive_entry_size_is_set\0"),
                mtime: f!(b"archive_entry_mtime\0"),
                filetype: f!(b"archive_entry_filetype\0"),
                is_encrypted: f!(b"archive_entry_is_encrypted\0"),
                _lib: lib,
            })
        }
    })
    .as_ref()
}

fn problem(message: String) -> anyhow::Error {
    let lower = message.to_lowercase();
    if lower.contains("passphrase") || lower.contains("encrypt") || lower.contains("password") {
        ArchiveError::Encrypted.into()
    } else {
        ArchiveError::Broken(message).into()
    }
}

/// 7z, RAR, CAB, ISO, LZH, ...: read front to back, so reading entries in order is cheap.
struct Library {
    lib: &'static Lib,
    src: Source,
    memory: Option<Arc<Vec<u8>>>,
    places: Vec<i64>, // each entry's place among all the archive's headers
    reader: Option<(Ptr, i64)>, // the archive, and the place of the last header read
}

// The archive handle is only used under the module's lock.
unsafe impl Send for Library {}

impl Library {
    fn new(src: Source) -> Result<Library> {
        let lib = library().ok_or(ArchiveError::NeedsWindows)?;
        let memory = match &src {
            Source::Bytes(b) => Some(b.clone()),
            Source::Path(_) => None,
        };
        Ok(Library { lib, src, memory, places: Vec::new(), reader: None })
    }

    fn open(&self) -> Result<Ptr> {
        let l = self.lib;
        unsafe {
            let a = (l.read_new)();
            if a.is_null() {
                return Err(broken("no memory"));
            }
            (l.support_filter_all)(a);
            (l.support_format_all)(a);
            let r = match (&self.memory, &self.src) {
                (Some(m), _) => (l.open_memory)(a, m.as_ptr() as *const c_void, m.len()),
                (None, Source::Path(p)) => {
                    let wide: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
                    (l.open_filename_w)(a, wide.as_ptr(), 1 << 16)
                }
                (None, Source::Bytes(_)) => unreachable!(),
            };
            if r != 0 {
                let message = self.error(a);
                (l.free)(a);
                return Err(problem(message));
            }
            Ok(a)
        }
    }

    fn error(&self, a: Ptr) -> String {
        unsafe {
            let m = (self.lib.error_string)(a);
            if m.is_null() {
                "unreadable archive".into()
            } else {
                std::ffi::CStr::from_ptr(m).to_string_lossy().into_owned()
            }
        }
    }

    fn drop_reader(&mut self) {
        if let Some((a, _)) = self.reader.take() {
            unsafe { (self.lib.free)(a) };
        }
    }
}

unsafe fn wide_str(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) }))
}

impl Container for Library {
    fn entries(&mut self) -> Result<Vec<Entry>> {
        let l = self.lib;
        let a = self.open()?;
        let mut out = Vec::new();
        let mut places = Vec::new();
        let mut entry: Ptr = std::ptr::null_mut();
        let mut place: i64 = -1;
        let result = (|| -> Result<()> {
            unsafe {
                while out.len() <= MAX_ENTRIES_BIG {
                    let r = (l.next_header)(a, &mut entry);
                    if r == EOF {
                        break;
                    }
                    if r < WARN {
                        return Err(problem(self.error(a)));
                    }
                    place += 1;
                    if (l.filetype)(entry) & 0o170000 != REGULAR {
                        continue;
                    }
                    let name = match wide_str((l.pathname_w)(entry)) {
                        Some(n) if !n.is_empty() => n,
                        _ => {
                            let raw = (l.pathname)(entry);
                            if raw.is_null() {
                                String::new()
                            } else {
                                std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
                            }
                        }
                    };
                    let size = if (l.size_is_set)(entry) != 0 { (l.size)(entry).max(0) as u64 } else { 0 };
                    let mut e = Entry::new(name, size, (l.mtime)(entry) as f64);
                    e.encrypted = (l.is_encrypted)(entry) != 0;
                    out.push(e);
                    places.push(place);
                }
            }
            Ok(())
        })();
        unsafe { (l.free)(a) };
        result?;
        self.places = places;
        Ok(out)
    }

    fn read_entry(&mut self, index: usize, limit: u64) -> Result<Vec<u8>> {
        let target = *self.places.get(index).ok_or(ArchiveError::NotFound)?;
        if self.reader.is_none_or(|(_, place)| place >= target) {
            self.drop_reader();
            self.reader = Some((self.open()?, -1));
        }
        let (a, mut place) = self.reader.unwrap();
        let l = self.lib;
        let result = (|| -> Result<Vec<u8>> {
            unsafe {
                let mut header: Ptr = std::ptr::null_mut();
                while place < target {
                    let r = (l.next_header)(a, &mut header);
                    if r == EOF {
                        return Err(ArchiveError::NotFound.into());
                    }
                    if r < WARN {
                        return Err(problem(self.error(a)));
                    }
                    place += 1;
                }
                self.reader = Some((a, place));
                let mut data = Vec::new();
                let mut buf = vec![0u8; 1 << 16];
                loop {
                    let n = (l.read_data)(a, buf.as_mut_ptr() as *mut c_void, buf.len());
                    if n == 0 {
                        break;
                    }
                    if n < 0 {
                        return Err(problem(self.error(a)));
                    }
                    data.extend_from_slice(&buf[..n as usize]);
                    if data.len() as u64 > limit {
                        break;
                    }
                }
                Ok(data)
            }
        })();
        if result.is_err() {
            self.drop_reader();
        }
        result
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        self.drop_reader();
    }
}

fn open_container(src: Source, label: &str) -> Result<Box<dyn Container>> {
    Ok(match container_type(label) {
        Some("zip") => match &src {
            Source::Path(p) => Box::new(Zip::new(BufReader::with_capacity(1 << 16, File::open(p)?))?),
            Source::Bytes(b) => Box::new(Zip::new(Cursor::new(Shared(b.clone())))?),
        },
        Some("tar") => Box::new(Tar::new(src)?),
        Some("single") => Box::new(Single::new(src, label)?),
        Some("library") => Box::new(Library::new(src)?),
        Some("mail") => {
            let data = src.bytes()?;
            Box::new(MailBox { mail: crate::mail::open_mail(&data).map_err(|e| broken(e.to_string()))? })
        }
        _ => return Err(anyhow!("not a container: {label}")),
    })
}

// ---- open containers, reused while indexing --------------------------------------------
// A session keeps containers open between reads (reading a ZIP's directory, or a 7z up to an
// entry, for every file inside would be slow). Outside one, everything is closed right away,
// so the app never holds your files open (Windows wouldn't let you move or delete them).

#[derive(PartialEq)]
struct Key {
    disk: String,
    mtime_ns: u128,
    size: u64,
    names: Vec<String>,
}

#[derive(Default)]
struct State {
    open: Vec<(Key, Open)>, // the most recently used last
    sessions: usize,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

/// Keeps containers open until dropped (and no other session is left).
pub struct Session(());

pub fn session() -> Session {
    STATE.lock().sessions += 1;
    Session(())
}

impl Drop for Session {
    fn drop(&mut self) {
        let mut st = STATE.lock();
        st.sessions -= 1;
        if st.sessions == 0 {
            st.open.clear();
        }
    }
}

/// Close every container kept open.
pub fn release() {
    STATE.lock().open.clear();
}

fn handle<'a>(st: &'a mut State, disk: &str, names: &[String]) -> Result<&'a mut Open> {
    let meta = std::fs::metadata(disk)?;
    let mtime_ns = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let key = Key {
        disk: disk.to_lowercase().replace('/', "\\"),
        mtime_ns,
        size: meta.len(),
        names: names.iter().map(|n| n.to_lowercase()).collect(),
    };
    if let Some(i) = st.open.iter().position(|(k, _)| *k == key) {
        let item = st.open.remove(i);
        st.open.push(item);
        return Ok(&mut st.open.last_mut().unwrap().1);
    }
    let container = match names.split_last() {
        Some((last, outer)) => {
            let data = handle(st, disk, outer)?.read(last, MAX_NESTED_BYTES)?;
            open_container(Source::Bytes(Arc::new(data)), last)?
        }
        None => open_container(Source::Path(PathBuf::from(disk)), disk)?,
    };
    st.open.push((key, Open::new(container)));
    while st.open.len() > MAX_OPEN {
        st.open.remove(0);
    }
    Ok(&mut st.open.last_mut().unwrap().1)
}

// ---- paths through containers ------------------------------------------------------------

fn label(disk: &str, names: &[String]) -> String {
    let mut out = disk.to_string();
    for n in names {
        out.push('\\');
        out.push_str(&n.replace('/', "\\"));
    }
    out
}

/// (the file on disk, [the names inside it]) for a path through containers: one name, or two
/// for a file in a container in a container. None for an ordinary path.
pub fn split(path: &str) -> Option<(String, Vec<String>)> {
    let parts: Vec<&str> = path.split('\\').collect();
    for k in 1..parts.len() {
        if container_type(parts[k - 1]).is_none() {
            continue;
        }
        let prefix = parts[..k].join("\\");
        if !Path::new(&prefix).is_file() {
            continue;
        }
        let rest = &parts[k..];
        for j in 0..rest.len().saturating_sub(1) {
            if container_type(rest[j]).is_some() {
                let outer = rest[..=j].join("/");
                let inner = {
                    let mut st = STATE.lock();
                    let found = handle(&mut st, &prefix, &[]).and_then(|h| h.find(&outer)).map(|f| f.is_some());
                    if st.sessions == 0 {
                        st.open.clear();
                    }
                    found.unwrap_or(false)
                };
                if inner {
                    return Some((prefix, vec![outer, rest[j + 1..].join("/")]));
                }
            }
        }
        return Some((prefix, vec![rest.join("/")]));
    }
    None
}

/// True for files on disk and for paths into a container that exists.
pub fn exists(path: &str) -> bool {
    Path::new(path).exists() || split(path).is_some()
}

/// One entry's bytes. Refuses more than `limit`, whatever the container claims.
pub fn read(path: &str, limit: u64) -> Result<Vec<u8>> {
    let (disk, names) = split(path).ok_or(ArchiveError::NotFound)?;
    let _s = session();
    let mut st = STATE.lock();
    let (last, outer) = names.split_last().unwrap();
    handle(&mut st, &disk, outer)?.read(last, limit)
}

#[cfg(test)]
thread_local! {
    /// Tests: the containers walk() opened, in turn.
    pub static WALKED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Everything inside the container `disk` (a file on disk), and inside the containers in it that
/// look_inside(name) allows, if `nested`. What can't be searched comes with its reason. Stops
/// when `out` returns false.
pub fn walk(
    disk: &str,
    mtime: f64,
    look_inside: &dyn Fn(&str) -> bool,
    nested: bool,
    max_entries: usize,
    out: &mut dyn FnMut(Item) -> bool,
) {
    #[cfg(test)]
    WALKED.with(|w| w.borrow_mut().push(disk.to_string()));
    let _s = session();
    let too_deep = if nested { "nested_deep" } else { "nested_off" };
    walk_in(disk, &[], mtime, if nested { 2 } else { 1 }, look_inside, max_entries, too_deep, out);
}

#[allow(clippy::too_many_arguments)]
fn walk_in(
    disk: &str,
    names: &[String],
    mtime: f64,
    depth: u32,
    look_inside: &dyn Fn(&str) -> bool,
    max_entries: usize,
    too_deep: &'static str,
    out: &mut dyn FnMut(Item) -> bool,
) -> bool {
    let label = label(disk, names);
    let base = label.rsplit('\\').next().unwrap_or("").to_string();
    let listed = {
        let mut st = STATE.lock();
        handle(&mut st, disk, names).and_then(|h| h.entries())
    };
    let entries = match listed {
        Ok(e) => e,
        Err(e) => {
            let problem = match e.downcast_ref::<ArchiveError>() {
                Some(ArchiveError::Encrypted) => "encrypted",
                Some(ArchiveError::NeedsWindows) => "needs_windows",
                Some(ArchiveError::TooLarge) => "nested_large",
                _ if SPLIT.is_match(&base) => "split_archive",
                _ => "damaged_archive",
            };
            return out(Item { path: label, name: base, size: 0, mtime, problem: Some(problem) });
        }
    };
    if entries.len() > max_entries {
        return out(Item { path: label, name: base, size: 0, mtime, problem: Some("too_many") });
    }
    let mut seen = HashSet::new();
    for e in entries.iter() {
        let Some(parts) = parts_of(&e.name) else {
            let item = Item { path: format!("{label}\\{}", e.name), name: e.name.clone(), size: e.size, mtime, problem: Some("odd_name") };
            if !out(item) {
                return false;
            }
            continue;
        };
        let path = format!("{label}\\{}", parts.join("\\"));
        if !seen.insert(path.to_lowercase()) {
            continue; // the same name twice: the first one counts
        }
        let when = if e.mtime != 0.0 { e.mtime } else { mtime };
        let name = parts.last().unwrap().clone();
        let item = |problem| Item { path: path.clone(), name: name.clone(), size: e.size, mtime: when, problem };
        if e.encrypted {
            if !out(item(Some("encrypted"))) {
                return false;
            }
            continue;
        }
        if e.inline {
            if !out(item(Some("inline_image"))) {
                return false;
            }
            continue;
        }
        if !out(item(None)) {
            return false;
        }
        if container_type(&name).is_some() && look_inside(&name) {
            let go_on = if depth <= 1 {
                out(item(Some(too_deep)))
            } else if e.size > MAX_NESTED_BYTES {
                out(item(Some("nested_large")))
            } else {
                let mut inner = names.to_vec();
                inner.push(parts.join("/"));
                walk_in(disk, &inner, when, depth - 1, look_inside, max_entries, "nested_deep", out)
            };
            if !go_on {
                return false;
            }
        }
    }
    true
}

/// The entry as a temporary file with the same name, removed again when dropped.
pub struct Extracted {
    dir: PathBuf,
    pub file: PathBuf,
}

impl Drop for Extracted {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir(prefix: &str) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let dir = base.join(format!("{prefix}{}-{}-{stamp:x}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

pub fn extracted(path: &str, limit: u64) -> Result<Extracted> {
    let data = read(path, limit)?;
    let dir = temp_dir("quiekel-rs-")?;
    let file = dir.join(path.rsplit('\\').next().unwrap_or("file"));
    let out = Extracted { dir, file };
    std::fs::write(&out.file, data)?;
    Ok(out)
}

fn opened_dir() -> PathBuf {
    std::env::temp_dir().join("Quiekel Embed") // copies of opened files
}

/// A copy of the entry to open in its app (which needs a real file), in a temp folder.
pub fn open_copy(path: &str, limit: u64) -> Result<PathBuf> {
    let (disk, names) = split(path).ok_or(ArchiveError::NotFound)?;
    let stem = Path::new(&disk).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let hash = sha1_smol::Sha1::from(disk.to_lowercase().as_bytes()).digest().to_string();
    let mut target = opened_dir().join(format!("{stem}-{}", &hash[..8]));
    for n in &names {
        for p in parts_of(n).unwrap_or_else(|| vec!["file".into()]) {
            target.push(p); // parts_of never lets a name step outside
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, read(path, limit)?)?;
    Ok(target)
}

/// Copies of opened files from earlier sessions are no longer needed.
pub fn clear_opened() {
    let _ = std::fs::remove_dir_all(opened_dir());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn temp() -> PathBuf {
        temp_dir("quiekel-rs-test-").unwrap()
    }

    fn zip_with(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn types() {
        assert_eq!(container_type("a.tar.gz"), Some("tar"));
        assert_eq!(container_type("notes.txt.gz"), Some("single"));
        assert_eq!(container_type(r"C:\x\b.7Z"), Some("library"));
        assert_eq!(container_type("m.msg"), Some("mail"));
        assert_eq!(container_type("x.txt"), None);
        assert!(parts_of("../evil.txt").is_none() && parts_of("C:/x").is_none());
        assert_eq!(parts_of("./a//b\\c.txt").unwrap(), vec!["a", "b", "c.txt"]);
    }

    #[test]
    fn zip_in_zip_walk_and_read() {
        let dir = temp();
        let inner = zip_with(&[("deep.txt", b"inner text")]);
        let outer = zip_with(&[("docs/a.txt", b"hello"), ("inner.zip", &inner), ("x.bin", b"\0")]);
        let disk = dir.join("outer.zip");
        std::fs::write(&disk, outer).unwrap();
        let disk = disk.to_string_lossy().to_string();
        let mut items = Vec::new();
        walk(&disk, 5.0, &|_| true, true, MAX_ENTRIES, &mut |i| {
            items.push(i);
            true
        });
        let paths: Vec<String> = items.iter().map(|i| i.path[disk.len()..].to_string()).collect();
        assert_eq!(paths, vec![r"\docs\a.txt", r"\inner.zip", r"\inner.zip\deep.txt", r"\x.bin"]);
        assert_eq!(read(&format!(r"{disk}\docs\a.txt"), 100).unwrap(), b"hello");
        assert_eq!(read(&format!(r"{disk}\inner.zip\deep.txt"), 100).unwrap(), b"inner text");
        assert!(matches!(
            read(&format!(r"{disk}\docs\a.txt"), 3).unwrap_err().downcast_ref::<ArchiveError>(),
            Some(ArchiveError::TooLarge)
        ));
        let (d, names) = split(&format!(r"{disk}\inner.zip\deep.txt")).unwrap();
        assert_eq!((d, names), (disk.clone(), vec!["inner.zip".to_string(), "deep.txt".to_string()]));
        let copy = extracted(&format!(r"{disk}\docs\a.txt"), 100).unwrap();
        assert_eq!(std::fs::read(&copy.file).unwrap(), b"hello");
        let held = copy.dir.clone();
        drop(copy);
        assert!(!held.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tar_gz_and_single() {
        let dir = temp();
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in [("a.txt", &b"first"[..]), ("sub/b.txt", b"second")] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_700_000_000);
            h.set_cksum();
            builder.append_data(&mut h, name, data).unwrap();
        }
        let raw = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        let disk = dir.join("pack.tar.gz");
        std::fs::write(&disk, gz.finish().unwrap()).unwrap();
        let disk = disk.to_string_lossy().to_string();
        let mut items = Vec::new();
        walk(&disk, 1.0, &|_| true, true, MAX_ENTRIES, &mut |i| {
            items.push(i);
            true
        });
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].mtime, 1_700_000_000.0);
        {
            let _s = session();
            assert_eq!(read(&format!(r"{disk}\sub\b.txt"), 100).unwrap(), b"second");
            assert_eq!(read(&format!(r"{disk}\a.txt"), 100).unwrap(), b"first"); // going back
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"just notes").unwrap();
        let single = dir.join("notes.txt.gz");
        std::fs::write(&single, gz.finish().unwrap()).unwrap();
        let single = single.to_string_lossy().to_string();
        assert_eq!(read(&format!(r"{single}\notes.txt"), 100).unwrap(), b"just notes");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn damaged_and_too_many() {
        let dir = temp();
        let disk = dir.join("bad.zip");
        std::fs::write(&disk, b"not a zip at all").unwrap();
        let disk = disk.to_string_lossy().to_string();
        let mut items = Vec::new();
        walk(&disk, 1.0, &|_| true, true, MAX_ENTRIES, &mut |i| {
            items.push(i);
            true
        });
        assert_eq!(items[0].problem, Some("damaged_archive"));
        let many = dir.join("many.zip");
        std::fs::write(&many, zip_with(&[("a", b"1"), ("b", b"2"), ("c", b"3")])).unwrap();
        let mut items = Vec::new();
        walk(&many.to_string_lossy(), 1.0, &|_| true, true, 2, &mut |i| {
            items.push(i);
            true
        });
        assert_eq!(items[0].problem, Some("too_many"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What's inside, path -> why it can't be searched (None: it can).
    fn walked(path: &str, nested: bool, max_entries: usize) -> BTreeMap<String, Option<&'static str>> {
        let mut out = BTreeMap::new();
        walk(path, 0.0, &|_| true, nested, max_entries, &mut |i| {
            out.insert(i.path, i.problem);
            true
        });
        out
    }

    fn put(dir: &Path, name: &str, data: &[u8]) -> String {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p.to_string_lossy().to_string()
    }

    fn base64(data: &[u8]) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                out.push(if i <= c.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }

    #[test]
    fn lists_files_as_if_the_archive_were_a_folder() {
        let dir = temp();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for (name, data) in [("2019/strand.png", &b"\x89PNG"[..]), ("notes.txt", b"Sonnencreme kaufen"), ("../evil.txt", b"outside"), ("C:/abs.txt", b"drive letter")] {
            w.start_file(name, o).unwrap();
            w.write_all(data).unwrap();
        }
        w.add_directory("empty/", o).unwrap();
        let z = put(&dir, "Urlaub.zip", &w.finish().unwrap().into_inner());
        let expected = BTreeMap::from([
            (format!(r"{z}\2019\strand.png"), None),
            (format!(r"{z}\notes.txt"), None),
            (format!(r"{z}\../evil.txt"), Some("odd_name")),
            (format!(r"{z}\C:/abs.txt"), Some("odd_name")),
        ]);
        assert_eq!(walked(&z, true, MAX_ENTRIES), expected); // no folders, no tricks
        assert_eq!(split(&format!(r"{z}\2019\strand.png")), Some((z.clone(), vec!["2019/strand.png".to_string()])));
        assert_eq!(split(&dir.join("plain").join("file.txt").to_string_lossy()), None);
        assert!(exists(&format!(r"{z}\notes.txt")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn archives_inside_archives_are_looked_into_once() {
        let dir = temp();
        let deepest = zip_with(&[("deep.txt", b"ganz unten")]);
        let middle = zip_with(&[("brief.txt", b"Hallo"), ("noch.zip", &deepest)]);
        let z = put(&dir, "outer.zip", &zip_with(&[("innen/archiv.zip", &middle)]));
        let inner = format!(r"{z}\innen\archiv.zip");
        let found = walked(&z, true, MAX_ENTRIES);
        assert_eq!(found[&format!(r"{inner}\brief.txt")], None);
        assert_eq!(found[&format!(r"{inner}\noch.zip")], Some("nested_deep"));
        assert!(!found.contains_key(&format!(r"{inner}\noch.zip\deep.txt")));
        let names = vec!["innen/archiv.zip".to_string(), "brief.txt".to_string()];
        assert_eq!(split(&format!(r"{inner}\brief.txt")), Some((z.clone(), names)));
        assert_eq!(read(&format!(r"{inner}\brief.txt"), 100).unwrap(), b"Hallo");
        assert_eq!(walked(&z, false, MAX_ENTRIES)[&inner], Some("nested_off"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_compressed_files_are_one_file_inside() {
        let dir = temp();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"ein Logbuch").unwrap();
        let single = put(&dir, "log.txt.gz", &gz.finish().unwrap());
        let mut items = Vec::new();
        walk(&single, 0.0, &|_| true, true, MAX_ENTRIES, &mut |i| {
            items.push((i.path, i.size));
            true
        });
        assert_eq!(items, [(format!(r"{single}\log.txt"), 11)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seven_zip_through_windows_own_archive_library() {
        if library().is_none() {
            return; // needs Windows' archive library (Windows 11, 2023 and later)
        }
        let dir = temp();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        put(&work, "rezept.txt", b"Apfelkuchen");
        put(&work, "zweites.txt", b"Zweite Datei");
        let seven = dir.join("rezepte.7z").to_string_lossy().to_string();
        let tar = Path::new(&std::env::var("SystemRoot").unwrap()).join("System32").join("tar.exe");
        let made = std::process::Command::new(tar).args(["-a", "-cf", &seven, "rezept.txt", "zweites.txt"]).current_dir(&work).status();
        assert!(made.unwrap().success());
        let found: Vec<String> = walked(&seven, true, MAX_ENTRIES).into_keys().collect();
        assert_eq!(found, [format!(r"{seven}\rezept.txt"), format!(r"{seven}\zweites.txt")]);
        {
            let _s = session(); // in any order
            assert_eq!(read(&format!(r"{seven}\zweites.txt"), 100).unwrap(), b"Zweite Datei");
            assert_eq!(read(&format!(r"{seven}\rezept.txt"), 100).unwrap(), b"Apfelkuchen");
            assert!(!STATE.lock().open.is_empty());
        }
        assert!(STATE.lock().open.is_empty()); // nothing kept open afterwards (Windows would lock the file)
        let in_zip = put(&dir, "mit7z.zip", &zip_with(&[("rezepte.7z", &std::fs::read(&seven).unwrap())]));
        assert_eq!(read(&format!(r"{in_zip}\rezepte.7z\rezept.txt"), 100).unwrap(), b"Apfelkuchen");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn password_protected_entries() {
        let dir = temp();
        let mut data = zip_with(&[("geheim.txt", b"x"), ("offen.txt", b"y")]);
        for (header, flags_at) in [(&b"PK\x03\x04"[..], 6), (&b"PK\x01\x02"[..], 8)] {
            // mark the first entry encrypted
            let at = data.windows(4).position(|w| w == header).unwrap();
            data[at + flags_at] |= 0x1;
        }
        let z = put(&dir, "teils.zip", &data);
        let expected = BTreeMap::from([(format!(r"{z}\geheim.txt"), Some("encrypted")), (format!(r"{z}\offen.txt"), None)]);
        assert_eq!(walked(&z, true, MAX_ENTRIES), expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn names_from_windows_own_zip_folders() {
        let oem = unsafe { windows::Win32::Globalization::GetOEMCP() };
        if oem != 437 && oem != 850 {
            return; // another OEM code page
        }
        let dir = temp();
        let mut data = zip_with(&[("MXrz.txt", b"Rechnung")]);
        while let Some(at) = data.windows(8).position(|w| w == b"MXrz.txt") {
            data[at + 1] = 0x84; // "März" in code page 437 or 850, as Windows' own ZIP folders write it
        }
        let z = put(&dir, "rechnungen.zip", &data);
        assert_eq!(walked(&z, true, MAX_ENTRIES), BTreeMap::from([(format!(r"{z}\März.txt"), None)]));
        assert_eq!(read(&format!(r"{z}\März.txt"), 100).unwrap(), b"Rechnung");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn email_attachments_are_files_inside_the_email() {
        let dir = temp();
        let mut eml = b"From: a@example.com\r\nSubject: Anhang\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n\
            --B\r\nContent-Type: multipart/related; boundary=R\r\n\r\n\
            --R\r\nContent-Type: text/html\r\n\r\n<p>Hallo <img src=\"cid:logo1\"></p>\r\n\
            --R\r\nContent-Type: image/png\r\nContent-ID: <logo1>\r\nContent-Disposition: inline; filename=logo.png\r\n\
            Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R--\r\n\
            --B\r\nContent-Type: application/zip\r\nContent-Disposition: attachment; filename=\"vertrag.zip\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n"
            .to_vec();
        eml.extend(base64(&zip_with(&[("vertrag.txt", b"Vertrag")])).as_bytes());
        eml.extend(b"\r\n--B--\r\n");
        let e = put(&dir, "Unterlagen.eml", &eml);
        let expected = BTreeMap::from([
            (format!(r"{e}\logo.png"), Some("inline_image")),
            (format!(r"{e}\vertrag.zip"), None),
            (format!(r"{e}\vertrag.zip\vertrag.txt"), None),
        ]);
        assert_eq!(walked(&e, true, MAX_ENTRIES), expected);
        assert_eq!(read(&format!(r"{e}\vertrag.zip\vertrag.txt"), 100).unwrap(), b"Vertrag");
        let msg = dir.join("Angebot.msg");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("Angebot.msg"), &msg).unwrap();
        let msg = msg.to_string_lossy().to_string();
        assert_eq!(walked(&msg, true, MAX_ENTRIES), BTreeMap::from([(format!(r"{msg}\preise.txt"), None)]));
        assert_eq!(read(&format!(r"{msg}\preise.txt"), 10_000).unwrap(), b"Ziegel ".repeat(700));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opening_copies_just_that_file_to_a_temp_folder() {
        let dir = temp();
        let z = put(&dir, "docs.zip", &zip_with(&[("a/b/letter.txt", b"Hallo")]));
        let copy = open_copy(&format!(r"{z}\a\b\letter.txt"), 1000).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "Hallo");
        assert!(copy.starts_with(opened_dir()) && copy.ends_with(r"a\b\letter.txt"));
        let _ = std::fs::remove_dir_all(copy.parent().unwrap().parent().unwrap().parent().unwrap());
        let tmp = extracted(&format!(r"{z}\a\b\letter.txt"), 1000).unwrap();
        assert!(tmp.file.ends_with("letter.txt") && std::fs::read_to_string(&tmp.file).unwrap() == "Hallo");
        let file = tmp.file.clone();
        drop(tmp);
        assert!(!file.exists()); // temporary copies are cleaned up
        let _ = std::fs::remove_dir_all(dir);
    }
}
