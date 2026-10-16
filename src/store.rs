//! Persistent state, in one SQLite file: folders, files, settings, keyword search (FTS5) and the
//! passages' vectors.
//!
//! Searches don't scan the vectors on disk: every passage's vector is also kept in memory
//! (`Vectors`, in half precision: half a KB per passage), which turns a search into a few
//! milliseconds of maths. While they're being read at start-up, and for indexes too big to keep,
//! searches read the vectors from disk.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use half::f16;
use half::slice::HalfFloatSliceExt;
use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::config::{self, EMBED_DIM};
use crate::filetypes::Report;
use crate::search::{KeywordHit, VectorHit};

/// Bump when what goes into the index changes (e.g. how titles are built): forces a re-index.
pub const INDEX_FORMAT: i64 = 3;

/// Bump when the readers get documents' text better in a way worth reading the affected ones
/// again for, without indexing everything anew (see Store::reread_documents). 2: PDFium's mark
/// inside words split at a line end (U+0002), other control codes, symbol fonts' glyphs (the
/// private use area).
pub const DOCUMENT_TEXT: &str = "2";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE COLLATE NOCASE,
    watch INTEGER NOT NULL DEFAULT 1,
    added_at REAL NOT NULL,
    last_scan_at REAL,
    report TEXT
);
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    path TEXT NOT NULL UNIQUE COLLATE NOCASE,
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    chunks INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    indexed_at REAL NOT NULL,
    muted INTEGER NOT NULL DEFAULT 0
);
-- A folder's files, by status (its unreadable or skipped ones without visiting the rest).
DROP INDEX IF EXISTS files_folder;
CREATE INDEX IF NOT EXISTS files_folder_status ON files(folder_id, status);
-- Archives and emails as last looked into: unchanged ones aren't opened again.
CREATE TABLE IF NOT EXISTS containers (
    path TEXT PRIMARY KEY COLLATE NOCASE,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    types TEXT NOT NULL,
    report TEXT NOT NULL
);
-- Keyword search over file names (+ their folders) and passage text.
-- rowid = file_id << 10 | chunk
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    name, text, tokenize = 'unicode61 remove_diacritics 2'
);
-- How many passages hold each word (read from the keyword index itself; nothing is stored).
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vocab USING fts5vocab(chunks_fts, 'row');
-- The passages' vectors (EMBED_DIM float32 each), id as in chunks_fts; page: the page a passage
-- starts on, where the file has pages (added in 1.2: see add_columns).
CREATE TABLE IF NOT EXISTS vectors (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    vector BLOB NOT NULL,
    page INTEGER
);
CREATE INDEX IF NOT EXISTS vectors_folder ON vectors(folder_id);
-- Folders inside an indexed folder that you left out (scans pass them by).
CREATE TABLE IF NOT EXISTS left_out (
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    path TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY (folder_id, path)
);
";

pub const FTS_SHIFT: i64 = 10; // up to 1024 chunks per file in the keyword index
pub const REMOVE_STEP: usize = 500; // files taken out of the index in one transaction
const CHUNK_MASK: i64 = (1 << FTS_SHIFT) - 1;

pub fn kind_code(kind: &str) -> i8 {
    match kind {
        "doc" => 0,
        "text" => 1,
        "code" => 2,
        "image" => 3,
        _ => 9,
    }
}


fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The last part of a path.
fn split_last(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// What a folder in a folder list is, when it's a file the app looks into: "archive" or "mail".
fn dir_kind(name: &str) -> Option<&'static str> {
    match crate::archive::container_type(name)? {
        "mail" => Some("mail"),
        _ => Some("archive"),
    }
}

/// Columns newer versions added to tables an older version made (SQLite has no ADD COLUMN IF NOT
/// EXISTS): files indexed before simply have none there.
fn add_columns(db: &Connection) -> Result<()> {
    let has = |table: &str, column: &str| -> rusqlite::Result<bool> {
        db.query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?"), [column], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
    };
    if !has("vectors", "page")? {
        db.execute_batch("ALTER TABLE vectors ADD COLUMN page INTEGER")?;
    }
    Ok(())
}

fn fts_range(file_id: i64) -> (i64, i64) {
    (file_id << FTS_SHIFT, (file_id << FTS_SHIFT) | CHUNK_MASK)
}

/// 'C:\x' -> 'C:\x\', for matching the paths of everything inside a folder.
fn dir_prefix(path: &str) -> String {
    format!("{}\\", path.trim_end_matches(['\\', '/']))
}

/// The paths inside a folder, as a range the paths' index can look up (case-insensitive, as the
/// column): from 'C:\x\' up to 'C:\x]' (']' comes right after '\').
fn inside(path: &str) -> (String, String) {
    let lo = dir_prefix(path);
    let hi = format!("{}]", &lo[..lo.len() - 1]);
    (lo, hi)
}

/// A path as Windows compares paths: backslashes, lowercase.
pub fn normcase(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8], out: &mut Vec<f32>) {
    out.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
}

#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    for (x, y) in a.chunks_exact(8).zip(b.chunks_exact(8)) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let rest: f32 = a.chunks_exact(8).remainder().iter().zip(b.chunks_exact(8).remainder()).map(|(x, y)| x * y).sum();
    acc.iter().sum::<f32>() + rest
}

/// A vector kept in half precision against one in full: with the processor's vector instructions
/// (AVX2, FMA and F16C, most PCs since 2013), else through a conversion first.
#[inline]
fn dot16(a: &[f16], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    if *VECTOR_UNITS {
        // SAFETY: the processor has the instructions (checked once).
        return unsafe { dot16_avx2(a, b) };
    }
    let mut row = [0f32; EMBED_DIM];
    let row = &mut row[..a.len().min(EMBED_DIM)];
    a[..row.len()].convert_to_f32_slice(row);
    dot(row, b)
}

#[cfg(target_arch = "x86_64")]
static VECTOR_UNITS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") && is_x86_feature_detected!("f16c")
});

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma,f16c")]
unsafe fn dot16_avx2(a: &[f16], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr() as *const u16, b.as_ptr());
    // SAFETY: every load stays below n, inside both slices.
    unsafe {
        let mut s = [_mm256_setzero_ps(); 4];
        let mut i = 0;
        while i + 32 <= n {
            for (k, acc) in s.iter_mut().enumerate() {
                let x = _mm256_cvtph_ps(_mm_loadu_si128(pa.add(i + 8 * k) as *const __m128i));
                *acc = _mm256_fmadd_ps(x, _mm256_loadu_ps(pb.add(i + 8 * k)), *acc);
            }
            i += 32;
        }
        let t = _mm256_add_ps(_mm256_add_ps(s[0], s[1]), _mm256_add_ps(s[2], s[3]));
        let x = _mm_add_ps(_mm256_castps256_ps128(t), _mm256_extractf128_ps(t, 1));
        let x = _mm_add_ps(x, _mm_movehl_ps(x, x));
        let x = _mm_add_ss(x, _mm_shuffle_ps(x, x, 1));
        let mut sum = _mm_cvtss_f32(x);
        while i < n {
            sum += a[i].to_f32() * b[i];
            i += 1;
        }
        sum
    }
}

/// The k best of what comes along, by score (best first once full).
struct Best {
    k: usize,
    items: Vec<(f32, usize, usize)>,
}

impl Best {
    fn new(k: usize) -> Best {
        Best { k, items: Vec::with_capacity(k + 1) }
    }

    /// Whether a score would make it in.
    #[inline]
    fn wants(&self, score: f32) -> bool {
        self.items.len() < self.k || score > self.items[self.k - 1].0
    }

    fn add(&mut self, item: (f32, usize, usize)) {
        if self.items.len() < self.k {
            self.items.push(item);
            if self.items.len() == self.k {
                self.items.sort_by(|x, y| y.0.total_cmp(&x.0));
            }
        } else if item.0 > self.items[self.k - 1].0 {
            let at = self.items.partition_point(|x| x.0 >= item.0);
            self.items.insert(at, item);
            self.items.pop();
        }
    }

    fn into_sorted(mut self) -> Vec<(f32, usize, usize)> {
        self.items.sort_by(|x, y| y.0.total_cmp(&x.0));
        self.items
    }
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt();
    let n = if n > 0.0 { n } else { 1.0 };
    v.iter().map(|x| x / n).collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct FileRecord {
    pub id: i64,
    pub folder_id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: f64,
    pub kind: String,
    pub status: String,
    pub chunks: i64,
    pub error: Option<String>,
}

impl FileRecord {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get(0)?,
            folder_id: r.get(1)?,
            path: r.get(2)?,
            size: r.get(3)?,
            mtime: r.get(4)?,
            kind: r.get(5)?,
            status: r.get(6)?,
            chunks: r.get(7)?,
            error: r.get(8)?,
        })
    }
}

const FILE_COLUMNS: &str = "id, folder_id, path, size, mtime, kind, status, chunks, error";

/// A folder with what's in it, as the page shows it.
#[derive(Clone, Debug, Serialize)]
pub struct Folder {
    pub id: i64,
    pub path: String,
    pub watch: i64,
    pub added_at: f64,
    pub last_scan_at: Option<f64>,
    pub files: i64,
    pub indexed: i64,
    pub pending: i64,
    pub errors: i64,
    pub unreadable: i64,
    pub skipped: i64,
    pub bytes: i64,
    pub chunks: i64,
    pub images: i64,
    // The rest of what's searched, by kind as File types counts them (pictures are `images`).
    pub docs: i64,
    pub texts: i64, // text and code
    pub mails: i64,
    pub unsearched: i64,
}

/// An email by its name (File types counts emails apart from the documents and texts they are).
const MAIL_LIKE: &str = "(x.path LIKE '%.eml' OR x.path LIKE '%.msg' OR x.path LIKE '%.mht' OR x.path LIKE '%.mhtml')";

#[derive(Clone, Debug)]
pub struct FolderRow {
    pub path: String,
}

/// An archive or email as last looked into.
#[derive(Clone, Debug)]
pub struct ContainerRow {
    pub path: String,
    pub size: i64,
    pub mtime: f64,
    pub types: String,
    pub report: Report,
}

/// A file as indexing leaves it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub folder_id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: f64,
    pub kind: String,
    pub status: String,
    pub chunks: i64,
    pub error: Option<String>,
    pub search_name: String,
}

/// One passage's vector, for save_files.
#[derive(Clone, Debug)]
pub struct VectorRow {
    pub path: String,
    pub folder_id: i64,
    pub chunk: i64,
    pub kind: String,
    pub text: String,
    pub vector: Vec<f32>,
    pub page: Option<i64>, // the page the passage starts on, where the file has pages
}

// ---- vectors in memory -------------------------------------------------------------------------

/// Rows of vectors added together (a save, or everything at start-up).
pub struct Block {
    ids: Vec<i64>,
    chunks: Vec<i32>,
    kinds: Vec<i8>,
    folders: Vec<i64>,
    vecs: Vec<f16>,
    alive: Vec<bool>,
}

impl Block {
    fn new(ids: Vec<i64>, chunks: Vec<i32>, kinds: Vec<i8>, folders: Vec<i64>, vecs: Vec<f16>) -> Block {
        let alive = vec![true; ids.len()];
        Block { ids, chunks, kinds, folders, vecs, alive }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn row(&self, i: usize) -> &[f16] {
        &self.vecs[i * EMBED_DIM..(i + 1) * EMBED_DIM]
    }
}

/// What a search is limited to.
#[derive(Default)]
pub struct Filter<'a> {
    pub kinds: Option<Vec<i8>>,
    pub folder_id: Option<i64>,
    pub file_ids: Option<&'a HashSet<i64>>,
}

impl Filter<'_> {
    fn allows(&self, kind: i8, folder: i64, file: i64) -> bool {
        self.kinds.as_ref().is_none_or(|k| k.contains(&kind))
            && self.folder_id.is_none_or(|f| f == 0 || f == folder)
            && self.file_ids.is_none_or(|s| s.contains(&file))
    }
}

/// Every passage's vector in memory, so a search is a few milliseconds of maths instead of a read
/// of the vectors on disk. The store keeps it in step with its own writes. Writes add small
/// blocks, merged into one now and then.
#[derive(Default)]
pub struct Vectors {
    pub ready: bool,
    pub rows: usize,
    blocks: Vec<Block>,
}

impl Vectors {
    pub const MAX_ROWS: usize = 500_000; // ~250 MB: bigger indexes are searched on disk instead
    const MAX_BLOCKS: usize = 48;

    fn fill(&mut self, block: Block) {
        self.rows = block.len();
        self.blocks = if block.len() > 0 { vec![block] } else { vec![] };
        self.ready = self.rows <= Self::MAX_ROWS;
        if !self.ready {
            self.blocks.clear();
        }
    }

    fn clear(&mut self) {
        self.blocks = Vec::new();
        self.rows = 0;
        self.ready = false;
    }

    fn add(&mut self, block: Block) {
        if !self.ready || block.len() == 0 {
            return;
        }
        self.rows += block.len();
        self.blocks.push(block);
        if self.rows > Self::MAX_ROWS {
            self.clear();
        } else if self.blocks.len() > Self::MAX_BLOCKS {
            self.merge();
        }
    }

    /// Rows of one folder now count in another (a folder taken over by one around it).
    fn refolder(&mut self, from: i64, to: i64) {
        for b in &mut self.blocks {
            b.folders.iter_mut().filter(|f| **f == from).for_each(|f| *f = to);
        }
    }

    /// Forget the rows `which(file_id, folder_id)` picks.
    fn drop_rows(&mut self, which: &dyn Fn(i64, i64) -> bool) {
        let mut dead = 0;
        for b in &mut self.blocks {
            for i in 0..b.ids.len() {
                if b.alive[i] && which(b.ids[i], b.folders[i]) {
                    b.alive[i] = false;
                    self.rows -= 1;
                }
                if !b.alive[i] {
                    dead += 1;
                }
            }
        }
        if dead > 1000.max(self.rows / 4) {
            self.merge();
        }
    }

    fn merge(&mut self) {
        let (mut ids, mut chunks, mut kinds, mut folders, mut vecs) = (vec![], vec![], vec![], vec![], vec![]);
        for b in &self.blocks {
            for i in 0..b.len() {
                if b.alive[i] {
                    ids.push(b.ids[i]);
                    chunks.push(b.chunks[i]);
                    kinds.push(b.kinds[i]);
                    folders.push(b.folders[i]);
                    vecs.extend_from_slice(b.row(i));
                }
            }
        }
        self.blocks = if ids.is_empty() { vec![] } else { vec![Block::new(ids, chunks, kinds, folders, vecs)] };
    }

    /// (file_id, chunk, kind code, cosine) of the closest passages, best first.
    fn search(&self, q: &[f32], limit: usize, filter: &Filter) -> Vec<(i64, i32, i8, f32)> {
        if limit == 0 {
            return Vec::new();
        }
        let mut best = Best::new(limit);
        for (bi, b) in self.blocks.iter().enumerate() {
            for i in 0..b.len() {
                if b.alive[i] && filter.allows(b.kinds[i], b.folders[i], b.ids[i]) {
                    let s = dot16(b.row(i), q);
                    if best.wants(s) {
                        best.add((s, bi, i));
                    }
                }
            }
        }
        best.into_sorted()
            .into_iter()
            .map(|(s, bi, i)| {
                let b = &self.blocks[bi];
                (b.ids[i], b.chunks[i], b.kinds[i], s)
            })
            .collect()
    }

    /// (file ids, vectors) of these files' passages (or of all, for None).
    fn rows_of(&self, file_ids: Option<&HashSet<i64>>) -> (Vec<i64>, Vec<f32>) {
        let (mut ids, mut vecs) = (Vec::new(), Vec::new());
        for b in &self.blocks {
            for i in 0..b.len() {
                if b.alive[i] && file_ids.is_none_or(|s| s.contains(&b.ids[i])) {
                    ids.push(b.ids[i]);
                    let at = vecs.len();
                    vecs.resize(at + EMBED_DIM, 0.0);
                    b.row(i).convert_to_f32_slice(&mut vecs[at..]);
                }
            }
        }
        (ids, vecs)
    }
}

// ---- the store -----------------------------------------------------------------------------------

pub struct Store {
    db: Mutex<Connection>, // writes, and the indexer's own reads
    // The page's reads (searches, folders, details): in WAL mode they don't wait for a write
    // under way, nor a write for them.
    reader: Mutex<Connection>,
    dir: std::path::PathBuf, // the folder the index is in (the map's file goes next to it)
    pub vectors: RwLock<Vectors>,
    version: AtomicU64,
    structure: AtomicU64, // goes up when folders are added, removed or changed (not just their files)
    folders_cache: Mutex<Option<FoldersCache>>,
    passages_cache: Mutex<Option<(u64, i64)>>, // (version, passages in the keyword index)
    facts_cache: Mutex<Option<(u64, Value)>>,  // (version, the index in a few numbers)
    keep_vectors: AtomicBool,
    too_many: AtomicBool, // more passages than fit in memory: search them on disk
    loading: Mutex<()>,
    // Folders being removed: gone for the page already, their files go in steps (see
    // remove_folder_step). Kept in the meta table too, so a restart goes on with them.
    removing: Mutex<HashSet<i64>>,
}

/// The folders as last counted.
struct FoldersCache {
    version: u64,
    structure: u64,
    at: Instant,
    folders: Vec<Folder>,
}

// While only files change (indexing), the folders' counts are this old at most.
const FOLDERS_FRESH: std::time::Duration = if cfg!(test) { std::time::Duration::ZERO } else { std::time::Duration::from_secs(2) };

fn ids_sql(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

impl Store {
    pub fn open() -> Result<Store> {
        Self::open_at(&config::db_path())
    }

    pub fn open_at(path: &std::path::Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        // In WAL mode this is still crash-safe; a power cut may only undo the last saves, which
        // are then indexed again.
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.execute_batch(SCHEMA)?;
        add_columns(&db)?;
        let reader = Connection::open(path)?;
        reader.busy_timeout(std::time::Duration::from_secs(30))?;
        reader.pragma_update(None, "query_only", "ON")?;
        let store = Store {
            db: Mutex::new(db),
            reader: Mutex::new(reader),
            dir: path.parent().map(std::path::Path::to_path_buf).unwrap_or_default(),
            vectors: RwLock::new(Vectors::default()),
            version: AtomicU64::new(0),
            structure: AtomicU64::new(0),
            folders_cache: Mutex::new(None),
            passages_cache: Mutex::new(None),
            facts_cache: Mutex::new(None),
            keep_vectors: AtomicBool::new(true),
            too_many: AtomicBool::new(false),
            loading: Mutex::new(()),
            removing: Mutex::new(HashSet::new()),
        };
        store.check_index_format()?;
        let removing: Vec<i64> = store.meta_get("removing").and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
        store.removing.lock().extend(removing);
        Ok(store)
    }

    fn changed(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// The folders themselves changed (added, removed, watched, scanned): the page sees it at once.
    fn restructured(&self) {
        self.structure.fetch_add(1, Ordering::SeqCst);
        self.changed();
    }

    /// Where the map of the files is kept.
    pub fn map_path(&self) -> std::path::PathBuf {
        self.dir.join("map.bin")
    }

    /// Goes up with every change to the index.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Vectors from different models, sizes or formats can't be mixed: start over if any changed.
    fn check_index_format(&self) -> Result<()> {
        let current = format!("{}@{}/f{}", config::MODEL_ID, EMBED_DIM, INDEX_FORMAT);
        if self.meta_get("index").is_some_and(|v| v != current) {
            let db = self.db.lock();
            db.execute_batch("DELETE FROM files; DELETE FROM chunks_fts; DELETE FROM containers; DELETE FROM vectors;")?;
            let _ = std::fs::remove_file(self.map_path()); // its files are gone
            // What versions before 1.0 kept next to the index: their vectors, map and update note.
            let _ = std::fs::remove_dir_all(self.dir.join("vectors"));
            for old in ["map.npz", "update-result.json"] {
                let _ = std::fs::remove_file(self.dir.join(old));
            }
        }
        self.meta_set("index", &current);
        Ok(())
    }

    /// Documents whose saved passages hold what the readers now leave out (DOCUMENT_TEXT) are read
    /// again on their folder's next scan: their saved dates stop matching, and so do those of the
    /// archives and emails they're in. Once per DOCUMENT_TEXT; how many there were.
    pub fn reread_documents(&self) -> Result<usize> {
        if self.meta_get("document_text").as_deref() == Some(DOCUMENT_TEXT) {
            return Ok(0);
        }
        // Control codes but tabs and line ends, and the private use area.
        let odd: String = (1u8..32).filter(|c| ![9, 10, 13].contains(c)).map(char::from).collect();
        let pattern = format!("*[{odd}\u{e000}-\u{f8ff}]*");
        let mut db = self.db.lock();
        let paths: Vec<String> = {
            let mut st = db.prepare(&format!(
                "SELECT path FROM files WHERE kind = 'doc' AND id IN
                     (SELECT DISTINCT rowid >> {FTS_SHIFT} FROM chunks_fts WHERE text GLOB ?1)"
            ))?;
            st.query_map([&pattern], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        if !paths.is_empty() {
            let containers: HashMap<String, String> = {
                let mut st = db.prepare("SELECT path FROM containers")?;
                st.query_map([], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()).map(|p| (normcase(&p), p)).collect()
            };
            let tx = db.transaction()?;
            for path in &paths {
                tx.execute("UPDATE files SET mtime = 0 WHERE path = ?", [path])?;
                let lower = normcase(path);
                for (i, _) in lower.match_indices('\\') {
                    if let Some(container) = containers.get(&lower[..i]) {
                        tx.execute("UPDATE containers SET mtime = 0 WHERE path = ?", [container])?;
                    }
                }
            }
            tx.commit()?;
        }
        drop(db);
        self.meta_set("document_text", DOCUMENT_TEXT);
        Ok(paths.len())
    }

    // ---- vectors in memory ------------------------------------------------------------------

    /// Keep them in memory (filled now, in the background), or let them go and search the vectors
    /// on disk.
    pub fn keep_vectors_in_memory(self: &Arc<Self>, keep: bool) {
        self.keep_vectors.store(keep, Ordering::SeqCst);
        if keep {
            let me = self.clone();
            std::thread::Builder::new().name("vectors".into()).spawn(move || me.load_vectors()).ok();
        } else {
            *self.vectors.write() = Vectors::default();
        }
    }

    /// Whether the search data is kept in memory or searched on disk.
    #[cfg(test)]
    pub fn keeps_vectors_in_memory(&self) -> bool {
        self.keep_vectors.load(Ordering::SeqCst)
    }

    /// Read every vector into memory.
    pub fn load_vectors(&self) {
        if !self.keep_vectors.load(Ordering::SeqCst) || self.vectors.read().ready || self.too_many.load(Ordering::SeqCst) {
            return;
        }
        let _one = self.loading.lock();
        let started = Instant::now();
        let db = self.db.lock(); // (no write can slip in between)
        if self.vectors.read().ready {
            return;
        }
        let n: i64 = db.query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0)).unwrap_or(0);
        if n as usize > Vectors::MAX_ROWS {
            self.too_many.store(true, Ordering::SeqCst);
            log::info!("{n} passages: searching them on disk instead of in memory");
            return;
        }
        let n = n as usize;
        let (mut ids, mut chunks, mut kinds, mut folders) =
            (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        let mut vecs = Vec::with_capacity(n * EMBED_DIM);
        let removing = json!(self.removing()).to_string(); // (those folders' vectors are on their way out)
        let read = (|| -> rusqlite::Result<()> {
            let mut st = db.prepare("SELECT id, folder_id, kind, vector FROM vectors WHERE folder_id NOT IN (SELECT value FROM json_each(?))")?;
            let mut rows = st.query([removing])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                ids.push(id >> FTS_SHIFT);
                chunks.push((id & CHUNK_MASK) as i32);
                folders.push(r.get(1)?);
                kinds.push(r.get::<_, i64>(2)? as i8);
                vecs.extend(r.get_ref(3)?.as_blob()?.chunks_exact(4).map(|c| f16::from_f32(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))));
            }
            Ok(())
        })();
        if let Err(e) = read {
            log::warn!("Reading the vectors failed: {e}");
            return;
        }
        self.vectors.write().fill(Block::new(ids, chunks, kinds, folders, vecs));
        log::info!("{n} passages in memory in {:.2}s", started.elapsed().as_secs_f64());
    }

    fn vectors_ready(&self) -> bool {
        if self.keep_vectors.load(Ordering::SeqCst) && !self.vectors.read().ready && !self.too_many.load(Ordering::SeqCst) {
            self.load_vectors();
        }
        self.vectors.read().ready
    }

    /// What the vectors in memory take, in MB (0 when they're on disk).
    pub fn vectors_mb(&self) -> f64 {
        let v = self.vectors.read();
        if v.ready { (v.rows * EMBED_DIM * 2) as f64 / 1e6 } else { 0.0 }
    }

    /// The index in a few numbers, for Diagnostics: the files that can be searched, their passages,
    /// what it takes on disk, and what its vectors take in memory (0: they're searched on disk).
    pub fn facts(&self) -> Value {
        let version = self.version();
        if let Some((v, facts)) = self.facts_cache.lock().as_ref() {
            if *v == version {
                return facts.clone();
            }
        }
        let db = self.reader.lock();
        let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
        let (files, passages) = (count("SELECT COUNT(*) FROM files WHERE status = 'indexed'"), count("SELECT COUNT(*) FROM vectors"));
        drop(db);
        let size = |name: &str| std::fs::metadata(self.dir.join(name)).map(|m| m.len()).unwrap_or(0);
        let disk = size("state.db") + size("state.db-wal") + size("map.bin");
        let facts = json!({
            "files": files, "passages": passages, "disk_mb": (disk as f64 / 1e5).round() / 10.0,
            "memory_mb": (self.vectors_mb() * 10.0).round() / 10.0,
        });
        *self.facts_cache.lock() = Some((version, facts.clone()));
        facts
    }

    // ---- meta & settings ------------------------------------------------------------------

    pub fn meta_get(&self, key: &str) -> Option<String> {
        let db = self.db.lock();
        db.prepare_cached("SELECT value FROM meta WHERE key = ?")
            .and_then(|mut st| st.query_row([key], |r| r.get(0)).optional())
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) {
        let db = self.db.lock();
        let _ = db.execute(
            "INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    // ---- folders ------------------------------------------------------------------------------

    pub fn folders(&self) -> Vec<Folder> {
        let (version, structure) = (self.version(), self.structure.load(Ordering::SeqCst));
        if let Some(c) = self.folders_cache.lock().as_ref() {
            if c.version == version || (c.structure == structure && c.at.elapsed() < FOLDERS_FRESH) {
                return c.folders.clone();
            }
        }
        let db = self.reader.lock();
        let result = (|| -> rusqlite::Result<Vec<Folder>> {
            let mut st = db.prepare(&format!(
                "SELECT f.id, f.path, f.watch, f.added_at, f.last_scan_at, f.report,
                       COUNT(x.id) AS files,
                       COALESCE(SUM(x.status = 'indexed'), 0) AS indexed,
                       COALESCE(SUM(x.status = 'pending'), 0) AS pending,
                       COALESCE(SUM(x.status = 'error' AND x.muted = 0), 0) AS errors,
                       COALESCE(SUM(x.status = 'error'), 0) AS unreadable,
                       COALESCE(SUM(x.status = 'skipped'), 0) AS skipped,
                       COALESCE(SUM(CASE WHEN x.status = 'indexed' THEN x.size ELSE 0 END), 0) AS bytes,
                       COALESCE(SUM(x.chunks), 0) AS chunks,
                       COALESCE(SUM(x.kind = 'image' AND x.status = 'indexed'), 0) AS images,
                       COALESCE(SUM(x.status = 'indexed' AND x.kind = 'doc' AND NOT {MAIL_LIKE}), 0) AS docs,
                       COALESCE(SUM(x.status = 'indexed' AND x.kind IN ('text', 'code') AND NOT {MAIL_LIKE}), 0) AS texts,
                       COALESCE(SUM(x.status = 'indexed' AND x.kind <> 'image' AND {MAIL_LIKE}), 0) AS mails
                FROM folders f LEFT JOIN files x ON x.folder_id = f.id
                GROUP BY f.id ORDER BY f.path"
            ))?;
            let rows = st.query_map([], |r| {
                let report: Option<String> = r.get(5)?;
                let report: Report = report.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                let skipped: i64 = r.get(11)?;
                Ok(Folder {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    watch: r.get(2)?,
                    added_at: r.get(3)?,
                    last_scan_at: r.get(4)?,
                    files: r.get(6)?,
                    indexed: r.get(7)?,
                    pending: r.get(8)?,
                    errors: r.get(9)?,
                    unreadable: r.get(10)?,
                    skipped,
                    bytes: r.get(12)?,
                    chunks: r.get(13)?,
                    images: r.get(14)?,
                    docs: r.get(15)?,
                    texts: r.get(16)?,
                    mails: r.get(17)?,
                    // Everything that isn't searched: skipped files, and what the scan left out.
                    unsearched: skipped + report.total(),
                })
            })?;
            rows.collect()
        })()
        .unwrap_or_default();
        drop(db);
        let removing = self.removing.lock().clone();
        let result: Vec<Folder> = result.into_iter().filter(|f| !removing.contains(&f.id)).collect();
        *self.folders_cache.lock() = Some(FoldersCache { version, structure, at: Instant::now(), folders: result.clone() });
        result
    }

    pub fn folder(&self, folder_id: i64) -> Option<FolderRow> {
        if self.removing.lock().contains(&folder_id) {
            return None;
        }
        let db = self.reader.lock();
        db.query_row("SELECT path FROM folders WHERE id = ?", [folder_id], |r| Ok(FolderRow { path: r.get(0)? }))
        .optional()
        .ok()
        .flatten()
    }

    pub fn add_folder(&self, path: &str) -> Result<i64> {
        // A folder on its way out where this one goes (the same, inside it, or around it): its
        // files go first.
        let removing: Vec<i64> = self.removing.lock().iter().copied().collect();
        for id in removing {
            let old: Option<String> = self.db.lock().query_row("SELECT path FROM folders WHERE id = ?", [id], |r| r.get(0)).optional()?;
            if old.is_some_and(|old| crate::indexer::is_inside(path, &old) || crate::indexer::is_inside(&old, path)) {
                while !self.remove_folder_step(id, REMOVE_STEP)? {}
            }
        }
        let db = self.db.lock();
        db.execute("INSERT INTO folders (path, added_at) VALUES (?, ?)", params![path, now()])?;
        let id = db.last_insert_rowid();
        drop(db);
        self.restructured();
        Ok(id)
    }

    /// Remove a folder, all at once.
    #[cfg(test)]
    pub fn remove_folder(&self, folder_id: i64) -> Result<()> {
        self.begin_removing(folder_id);
        while !self.remove_folder_step(folder_id, REMOVE_STEP)? {}
        Ok(())
    }

    /// A folder is to go: for the page it's gone at once (from the folders, the searches and the
    /// map); remove_folder_step() then takes its files out of the index.
    pub fn begin_removing(&self, folder_id: i64) {
        let ids: Vec<i64> = {
            let mut r = self.removing.lock();
            r.insert(folder_id);
            r.iter().copied().collect()
        };
        self.meta_set("removing", &json!(ids).to_string());
        self.vectors.write().drop_rows(&|_, folder| folder == folder_id);
        self.restructured();
    }

    /// Folders on their way out.
    pub fn removing(&self) -> Vec<i64> {
        self.removing.lock().iter().copied().collect()
    }

    /// Takes up to `n` of a removed folder's files out of the index, with their keyword entries
    /// and vectors; true once none are left, and the folder itself is gone too.
    pub fn remove_folder_step(&self, folder_id: i64, n: usize) -> Result<bool> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        let ids: Vec<i64> = tx
            .prepare_cached("SELECT id FROM files WHERE folder_id = ? LIMIT ?")?
            .query_map(params![folder_id, n as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !ids.is_empty() {
            Self::forget(&tx, &ids)?;
            tx.commit()?;
            return Ok(false);
        }
        tx.execute("DELETE FROM vectors WHERE folder_id = ?", [folder_id])?; // (any left)
        tx.execute("DELETE FROM containers WHERE folder_id = ?", [folder_id])?;
        tx.execute("DELETE FROM folders WHERE id = ?", [folder_id])?;
        tx.commit()?;
        drop(db);
        let ids: Vec<i64> = {
            let mut r = self.removing.lock();
            r.remove(&folder_id);
            r.iter().copied().collect()
        };
        self.meta_set("removing", &json!(ids).to_string());
        self.vectors.write().drop_rows(&|_, folder| folder == folder_id); // (a group saved meanwhile)
        self.too_many.store(false, Ordering::SeqCst); // (maybe they fit in memory now)
        self.restructured();
        Ok(true)
    }

    /// Files, their keyword entries and their vectors, out of the index (in a transaction).
    fn forget(tx: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
        Self::delete_fts(tx, ids)?;
        let mut vectors = tx.prepare_cached("DELETE FROM vectors WHERE id BETWEEN ? AND ?")?;
        for &id in ids {
            let (lo, hi) = fts_range(id);
            vectors.execute([lo, hi])?;
        }
        tx.execute(&format!("DELETE FROM files WHERE id IN ({})", ids_sql(ids)), [])?;
        Ok(())
    }

    /// A folder added around an indexed one takes it over: its files (with their passages and
    /// vectors), what was found inside its archives and what's left out in it move to the new one,
    /// and it goes. Nothing is read again. (Its files keep the names they were indexed with, from
    /// the folder they were in.)
    pub fn take_over(&self, parent_id: i64, child_id: i64) -> Result<()> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        for table in ["files", "containers", "vectors", "left_out"] {
            tx.execute(&format!("UPDATE OR IGNORE {table} SET folder_id = ? WHERE folder_id = ?"), [parent_id, child_id])?;
        }
        tx.execute("DELETE FROM folders WHERE id = ?", [child_id])?;
        tx.commit()?;
        drop(db);
        self.vectors.write().refolder(child_id, parent_id);
        self.restructured();
        Ok(())
    }

    /// A folder that moved (another drive letter, a new name): it and everything in it get the new
    /// place. What's indexed stays: a scan then reads only what changed.
    pub fn relocate(&self, folder_id: i64, to: &str) -> Result<()> {
        let Some(old) = self.folder(folder_id).map(|f| f.path.trim_end_matches(['\\', '/']).to_string()) else {
            anyhow::bail!("no such folder");
        };
        let to = to.trim_end_matches(['\\', '/']);
        let (lo, hi) = inside(&old);
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        tx.execute("UPDATE folders SET path = ? WHERE id = ?", params![to, folder_id])?;
        let n = old.chars().count() as i64;
        for table in ["files", "containers", "left_out"] {
            tx.execute(
                &format!("UPDATE {table} SET path = ?1 || substr(path, ?2 + 1) WHERE folder_id = ?3 AND path >= ?4 AND path < ?5"),
                params![to, n, folder_id, lo, hi],
            )?;
        }
        tx.commit()?;
        drop(db);
        self.restructured();
        Ok(())
    }

    /// By category (as File types has them), the formats of the files in the index, and how many
    /// files each; archives by the archives and emails looked into.
    pub fn format_counts(&self) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>> {
        let mut out: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>> = Default::default();
        let db = self.reader.lock();
        let files: Vec<(String, String)> = db
            .prepare_cached("SELECT path, kind FROM files WHERE status IN ('indexed', 'pending')")
            .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .unwrap_or_default();
        let containers: Vec<String> = db
            .prepare_cached("SELECT path FROM containers")
            .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        drop(db);
        for (path, kind) in &files {
            let name = split_last(path);
            let ext = config::suffix(name);
            if ext.is_empty() {
                continue;
            }
            *out.entry(crate::filetypes::category(kind, name).to_string()).or_default().entry(ext).or_insert(0) += 1;
        }
        for path in &containers {
            let name = split_last(path);
            if crate::archive::container_type(name).is_some_and(|t| t != "mail") {
                *out.entry("archives".into()).or_default().entry(config::suffix(name)).or_insert(0) += 1;
            }
        }
        out
    }

    /// The folders inside a folder that aren't searched, because you left them out.
    pub fn left_out(&self, folder_id: i64) -> Vec<String> {
        let db = self.reader.lock();
        db.prepare_cached("SELECT path FROM left_out WHERE folder_id = ? ORDER BY path")
            .and_then(|mut st| st.query_map([folder_id], |r| r.get(0))?.collect())
            .unwrap_or_default()
    }

    /// Leave a folder inside a folder out (its files leave the index now; scans pass it by), or
    /// take it back in (the next scan reads it).
    pub fn set_left_out(&self, folder_id: i64, path: &str, out: bool) -> Result<()> {
        let path = path.trim_end_matches(['\\', '/']);
        if out {
            self.db.lock().execute("INSERT OR IGNORE INTO left_out (folder_id, path) VALUES (?, ?)", params![folder_id, path])?;
            let ids: Vec<i64> = {
                let (lo, hi) = inside(path);
                let db = self.db.lock();
                let mut st = db.prepare("SELECT id FROM files WHERE folder_id = ? AND path >= ? AND path < ?")?;
                st.query_map(params![folder_id, lo, hi], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            self.remove_file_ids(&ids)?;
            let (lo, hi) = inside(path);
            self.db.lock().execute("DELETE FROM containers WHERE folder_id = ? AND path >= ? AND path < ?", params![folder_id, lo, hi])?;
        } else {
            self.db.lock().execute("DELETE FROM left_out WHERE folder_id = ? AND path = ?", params![folder_id, path])?;
        }
        self.restructured();
        Ok(())
    }

    pub fn set_watch(&self, folder_id: i64, watch: bool) {
        let db = self.db.lock();
        let _ = db.execute("UPDATE folders SET watch = ? WHERE id = ?", params![watch as i64, folder_id]);
        drop(db);
        self.restructured();
    }

    /// A scan finished; `report`: why files were left out.
    pub fn mark_scanned(&self, folder_id: i64, report: Option<&Report>) {
        let db = self.db.lock();
        let report = report.map(|r| serde_json::to_string(r).unwrap_or_default());
        let _ = db.execute(
            "UPDATE folders SET last_scan_at = ?, report = COALESCE(?, report) WHERE id = ?",
            params![now(), report, folder_id],
        );
        drop(db);
        self.restructured();
    }

    /// The archives and emails in a folder as last looked into, by normcase'd path.
    pub fn containers(&self, folder_id: i64) -> HashMap<String, ContainerRow> {
        let db = self.db.lock();
        let rows = (|| -> rusqlite::Result<Vec<ContainerRow>> {
            let mut st = db.prepare("SELECT path, size, mtime, types, report FROM containers WHERE folder_id = ?")?;
            let rows = st.query_map([folder_id], |r| {
                let report: String = r.get(4)?;
                Ok(ContainerRow {
                    path: r.get(0)?,
                    size: r.get(1)?,
                    mtime: r.get(2)?,
                    types: r.get(3)?,
                    report: serde_json::from_str(&report).unwrap_or_default(),
                })
            })?;
            rows.collect()
        })()
        .unwrap_or_default();
        rows.into_iter().map(|r| (normcase(&r.path), r)).collect()
    }

    /// Remember what was found in archives and emails; `replace`: these are all of the folder's.
    pub fn save_containers(&self, folder_id: i64, rows: &[ContainerRow], replace: bool) -> Result<()> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        if replace {
            tx.execute("DELETE FROM containers WHERE folder_id = ?", [folder_id])?;
        }
        {
            let mut st = tx.prepare(
                "INSERT INTO containers (path, folder_id, size, mtime, types, report) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(path) DO UPDATE SET folder_id = excluded.folder_id, size = excluded.size,
                 mtime = excluded.mtime, types = excluded.types, report = excluded.report",
            )?;
            for r in rows {
                st.execute(params![r.path, folder_id, r.size, r.mtime, r.types, serde_json::to_string(&r.report)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Why a folder's files aren't searched: what its last scan left out, and the files that were
    /// skipped, by reason (a few of each).
    pub fn details(&self, folder_id: i64, examples: i64) -> Value {
        let db = self.reader.lock();
        let report: Value = db
            .query_row("SELECT report FROM folders WHERE id = ?", [folder_id], |r| r.get::<_, Option<String>>(0))
            .optional()
            .ok()
            .flatten()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}));
        let mut skipped = Vec::new();
        let counts: Vec<(Option<String>, i64)> = db
            .prepare(
                "SELECT error, COUNT(*) AS n FROM files WHERE folder_id = ? AND status = 'skipped'
                 GROUP BY error ORDER BY n DESC",
            )
            .and_then(|mut st| st.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .unwrap_or_default();
        for (error, n) in counts {
            let files: Vec<Value> = db
                .prepare(
                    "SELECT id, path FROM files WHERE folder_id = ? AND status = 'skipped' AND error IS ?
                     ORDER BY indexed_at DESC LIMIT ?",
                )
                .and_then(|mut st| {
                    st.query_map(params![folder_id, error, examples], |r| {
                        Ok(json!({"id": r.get::<_, i64>(0)?, "path": r.get::<_, String>(1)?}))
                    })?
                    .collect()
                })
                .unwrap_or_default();
            skipped.push(json!({"reason": error, "n": n, "examples": files}));
        }
        json!({"report": report, "skipped": skipped})
    }

    // ---- files -------------------------------------------------------------------------------------

    pub fn file(&self, file_id: i64) -> Option<FileRecord> {
        let db = self.reader.lock();
        db.prepare_cached(&format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?"))
            .and_then(|mut st| st.query_row([file_id], FileRecord::from_row).optional())
            .ok()
            .flatten()
    }

    /// The start of a file's text, as shown in search results ("" for pictures).
    pub fn first_passage(&self, file_id: i64) -> String {
        let db = self.reader.lock();
        db.prepare_cached("SELECT text FROM chunks_fts WHERE rowid = ?")
            .and_then(|mut st| st.query_row([file_id << FTS_SHIFT], |r| r.get(0)).optional())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// PDFs indexed before passages knew their page (rows without one), a few at a time: (id, path,
    /// passages). Not those inside archives or emails (Indexer::backfill_pages gives them 0).
    pub fn pdfs_without_pages(&self, limit: usize) -> Vec<(i64, String, i64)> {
        let db = self.db.lock();
        let sql = format!(
            "SELECT f.id, f.path, f.chunks FROM files f WHERE f.status = 'indexed' AND f.kind = 'doc' AND f.chunks > 0
               AND lower(f.path) LIKE '%.pdf'
               AND EXISTS (SELECT 1 FROM vectors v WHERE v.id BETWEEN (f.id << {FTS_SHIFT}) AND ((f.id << {FTS_SHIFT}) | {CHUNK_MASK})
                           AND v.page IS NULL)
             LIMIT ?"
        );
        db.prepare(&sql)
            .and_then(|mut st| st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect())
            .unwrap_or_default()
    }

    /// A file's passages' pages, in passage order (0: not known, so it isn't asked for again).
    pub fn set_pages(&self, file_id: i64, pages: &[i64]) -> Result<()> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        {
            let mut st = tx.prepare_cached("UPDATE vectors SET page = ? WHERE id = ?")?;
            for (chunk, page) in pages.iter().enumerate() {
                st.execute(params![page, (file_id << FTS_SHIFT) | chunk as i64])?;
            }
            // (passages past the ones given, if any: not known either)
            let (lo, hi) = fts_range(file_id);
            tx.execute("UPDATE vectors SET page = 0 WHERE id BETWEEN ? AND ? AND page IS NULL", [lo, hi])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The page each passage starts on (keys: file_id << FTS_SHIFT | chunk), where it's known.
    pub fn pages(&self, keys: &[i64]) -> HashMap<i64, i64> {
        let mut out = HashMap::new();
        let db = self.reader.lock();
        for part in keys.chunks(500) {
            let sql = format!("SELECT id, page FROM vectors WHERE page > 0 AND id IN ({})", ids_sql(part));
            if let Ok(mut st) = db.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
                    out.extend(rows.flatten());
                }
            }
        }
        out
    }

    pub fn files_by_ids(&self, ids: &[i64]) -> HashMap<i64, FileRecord> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return out;
        }
        let db = self.reader.lock();
        for part in ids.chunks(500) {
            let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id IN ({})", ids_sql(part));
            if let Ok(mut st) = db.prepare(&sql) {
                if let Ok(rows) = st.query_map([], FileRecord::from_row) {
                    for f in rows.flatten() {
                        out.insert(f.id, f);
                    }
                }
            }
        }
        drop(db);
        let removing = self.removing.lock();
        if !removing.is_empty() {
            out.retain(|_, f| !removing.contains(&f.folder_id)); // (on their way out)
        }
        out
    }

    /// Every file with vectors, by id: (id, mtime, size).
    pub fn map_files(&self) -> Vec<(i64, f64, f64)> {
        let removing = json!(self.removing()).to_string();
        let db = self.reader.lock();
        db.prepare(
            "SELECT id, mtime, size FROM files WHERE status = 'indexed' AND chunks > 0
             AND folder_id NOT IN (SELECT value FROM json_each(?)) ORDER BY id",
        )
        .and_then(|mut st| st.query_map([removing], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as f64)))?.collect())
        .unwrap_or_default()
    }

    pub fn map_file_count(&self) -> i64 {
        let removing = json!(self.removing()).to_string();
        let db = self.reader.lock();
        db.query_row(
            "SELECT COUNT(*) FROM files WHERE status = 'indexed' AND chunks > 0
             AND folder_id NOT IN (SELECT value FROM json_each(?))",
            [removing],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// path -> (id, size, mtime, status) for every known file in a folder.
    pub fn file_signatures(&self, folder_id: i64) -> HashMap<String, (i64, i64, f64, String)> {
        let db = self.db.lock();
        db.prepare("SELECT id, path, size, mtime, status FROM files WHERE folder_id = ?")
            .and_then(|mut st| {
                st.query_map([folder_id], |r| Ok((r.get::<_, String>(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?))))?
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (id, path) of the known files inside a folder, or inside an archive or an email.
    pub fn files_inside(&self, path: &str) -> Vec<(i64, String)> {
        let (lo, hi) = inside(path);
        let db = self.db.lock();
        db.prepare_cached("SELECT id, path FROM files WHERE path >= ? AND path < ?")
            .and_then(|mut st| st.query_map(params![lo, hi], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .unwrap_or_default()
    }

    /// Files just found and not indexed yet: searchable by name right away, by content once
    /// indexed. files: (path, size, mtime, kind, name words). Files already known are left alone.
    pub fn add_pending(&self, folder_id: i64, files: &[(String, i64, f64, String, String)]) -> Result<()> {
        let at = now();
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        let mut added = 0;
        {
            let mut insert = tx.prepare(
                "INSERT INTO files (folder_id, path, size, mtime, kind, status, chunks, error, indexed_at)
                 VALUES (?, ?, ?, ?, ?, 'pending', 0, NULL, ?)
                 ON CONFLICT(path) DO NOTHING RETURNING id",
            )?;
            let mut fts = tx.prepare("INSERT INTO chunks_fts (rowid, name, text) VALUES (?, ?, '')")?;
            for (path, size, mtime, kind, name) in files {
                let id: Option<i64> =
                    insert.query_row(params![folder_id, path, size, mtime, kind, at], |r| r.get(0)).optional()?;
                if let Some(id) = id {
                    fts.execute(params![id << FTS_SHIFT, name])?;
                    added += 1;
                }
            }
        }
        tx.commit()?;
        drop(db);
        if added > 0 {
            self.changed();
        }
        Ok(())
    }

    /// (id, size, mtime, status) of a known file.
    pub fn signature(&self, path: &str) -> Option<(i64, i64, f64, String)> {
        let db = self.db.lock();
        db.prepare_cached("SELECT id, size, mtime, status FROM files WHERE path = ?")
            .and_then(|mut st| st.query_row([path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional())
            .ok()
            .flatten()
    }

    /// A folder's unreadable files, newest first; "seen": its warning was looked at.
    pub fn errors(&self, folder_id: i64, limit: i64) -> Vec<Value> {
        let db = self.reader.lock();
        db.prepare(
            "SELECT id, path, error, muted FROM files WHERE folder_id = ? AND status = 'error'
             ORDER BY muted, indexed_at DESC LIMIT ?",
        )
        .and_then(|mut st| {
            st.query_map(params![folder_id, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "path": r.get::<_, String>(1)?,
                    "error": r.get::<_, Option<String>>(2)?, "seen": r.get::<_, i64>(3)? != 0,
                }))
            })?
            .collect()
        })
        .unwrap_or_default()
    }

    /// A folder's unreadable files were looked at: no more warning for them.
    pub fn errors_seen(&self, folder_id: i64) {
        let db = self.db.lock();
        let _ = db.execute("UPDATE files SET muted = 1 WHERE folder_id = ? AND status = 'error' AND muted = 0", [folder_id]);
        drop(db);
        self.restructured();
    }

    /// Replace the vectors and keyword entries of the given files and upsert their records.
    pub fn save_files(&self, entries: &[Entry], vector_rows: &[VectorRow]) -> Result<()> {
        let at = now();
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        // The folder may have been removed while this group was being embedded.
        let mut live: HashSet<i64> = {
            let mut st = tx.prepare("SELECT id FROM folders")?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        live.retain(|f| !self.removing.lock().contains(f)); // (or is on its way out)
        let entries: Vec<&Entry> = entries.iter().filter(|e| live.contains(&e.folder_id)).collect();
        let vector_rows: Vec<&VectorRow> = vector_rows.iter().filter(|r| live.contains(&r.folder_id)).collect();
        // Only files that had vectors need them deleted.
        let mut had = HashSet::new();
        for part in entries.chunks(500) {
            let marks = vec!["?"; part.len()].join(",");
            let mut st = tx.prepare(&format!("SELECT id FROM files WHERE chunks > 0 AND path IN ({marks})"))?;
            let found = st.query_map(params_from_iter(part.iter().map(|e| e.path.as_str())), |r| r.get::<_, i64>(0))?;
            for id in found {
                had.insert(id?);
            }
        }
        let mut ids: HashMap<String, i64> = HashMap::new();
        let mut names: HashMap<String, String> = HashMap::new();
        {
            let mut upsert = tx.prepare(
                "INSERT INTO files (folder_id, path, size, mtime, kind, status, chunks, error, indexed_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(path) DO UPDATE SET
                     folder_id = excluded.folder_id, size = excluded.size,
                     mtime = excluded.mtime, kind = excluded.kind, status = excluded.status,
                     chunks = excluded.chunks, error = excluded.error,
                     indexed_at = excluded.indexed_at,
                     muted = CASE WHEN excluded.status = 'error' THEN muted ELSE 0 END
                 RETURNING id",
            )?;
            for e in &entries {
                let id: i64 = upsert.query_row(
                    params![e.folder_id, e.path, e.size, e.mtime, e.kind, e.status, e.chunks, e.error, at],
                    |r| r.get(0),
                )?;
                ids.insert(e.path.clone(), id);
                names.insert(e.path.clone(), e.search_name.clone());
            }
        }
        let all: Vec<i64> = ids.values().copied().collect();
        Self::delete_fts(&tx, &all)?;
        let stale: Vec<i64> = all.iter().copied().filter(|i| had.contains(i)).collect();
        for part in stale.chunks(500) {
            for &id in part {
                let (lo, hi) = fts_range(id);
                tx.execute("DELETE FROM vectors WHERE id BETWEEN ? AND ?", [lo, hi])?;
            }
        }
        let (mut b_ids, mut b_chunks, mut b_kinds, mut b_folders, mut b_vecs) = (vec![], vec![], vec![], vec![], vec![]);
        {
            let mut vec_insert = tx.prepare("INSERT OR REPLACE INTO vectors (id, folder_id, kind, vector, page) VALUES (?, ?, ?, ?, ?)")?;
            let mut fts = tx.prepare("INSERT INTO chunks_fts (rowid, name, text) VALUES (?, ?, ?)")?;
            for r in &vector_rows {
                let Some(&file_id) = ids.get(&r.path) else { continue };
                let key = (file_id << FTS_SHIFT) | r.chunk;
                let kind = kind_code(&r.kind);
                vec_insert.execute(params![key, r.folder_id, kind as i64, to_blob(&r.vector), r.page])?;
                let name = if r.chunk == 0 { names.get(&r.path).cloned().unwrap_or_default() } else { String::new() };
                fts.execute(params![key, name, r.text])?;
                b_ids.push(file_id);
                b_chunks.push(r.chunk as i32);
                b_kinds.push(kind);
                b_folders.push(r.folder_id);
                b_vecs.extend(r.vector.iter().map(|&x| f16::from_f32(x)));
            }
        }
        {
            let mut v = self.vectors.write();
            if !stale.is_empty() {
                let gone: HashSet<i64> = stale.iter().copied().collect();
                v.drop_rows(&|f, _| gone.contains(&f));
            }
            if !b_ids.is_empty() {
                v.add(Block::new(b_ids, b_chunks, b_kinds, b_folders, b_vecs));
            }
        }
        tx.commit()?;
        drop(db);
        self.changed();
        Ok(())
    }

    /// Forget files, and everything below any path that was a directory (or an archive).
    pub fn remove_paths(&self, paths: &[String]) -> Result<()> {
        let mut ids = Vec::new();
        {
            let db = self.db.lock();
            let mut find = db.prepare_cached("SELECT id FROM files WHERE path = ? OR (path >= ? AND path < ?)")?;
            let mut forget = db.prepare_cached("DELETE FROM containers WHERE path = ? OR (path >= ? AND path < ?)")?;
            for p in paths {
                let (lo, hi) = inside(p);
                for id in find.query_map(params![p, lo, hi], |r| r.get::<_, i64>(0))? {
                    ids.push(id?);
                }
                forget.execute(params![p, lo, hi])?;
            }
        }
        if !ids.is_empty() {
            self.remove_file_ids(&ids)?;
        }
        Ok(())
    }

    /// Forget files: in steps of REMOVE_STEP, each its own transaction, so a big one doesn't hold
    /// up the other writes (a file left over by a crash is forgotten again by the next scan).
    pub fn remove_file_ids(&self, ids: &[i64]) -> Result<()> {
        let gone: HashSet<i64> = ids.iter().copied().collect();
        self.vectors.write().drop_rows(&|f, _| gone.contains(&f));
        for part in ids.chunks(REMOVE_STEP) {
            let mut db = self.db.lock();
            let tx = db.transaction()?;
            Self::forget(&tx, part)?;
            tx.commit()?;
        }
        self.changed();
        self.too_many.store(false, Ordering::SeqCst); // (maybe they fit in memory now)
        Ok(())
    }

    fn delete_fts(db: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
        let mut st = db.prepare("DELETE FROM chunks_fts WHERE rowid BETWEEN ? AND ?")?;
        for &id in ids {
            let (lo, hi) = fts_range(id);
            st.execute([lo, hi])?;
        }
        Ok(())
    }

    // ---- search --------------------------------------------------------------------------------------

    /// The passages closest in meaning to `vector`, best first; optionally only of some kinds, one
    /// folder, or the given files.
    pub fn search(&self, vector: &[f32], limit: usize, kinds: &[&str], folder_id: Option<i64>, file_ids: Option<&[i64]>) -> Vec<VectorHit> {
        let q = normalized(vector);
        if file_ids.is_some_and(|f| f.is_empty()) {
            return Vec::new();
        }
        let set: Option<HashSet<i64>> = file_ids.map(|f| f.iter().copied().collect());
        let filter = Filter {
            kinds: (!kinds.is_empty()).then(|| kinds.iter().map(|k| kind_code(k)).collect()),
            folder_id,
            file_ids: set.as_ref(),
        };
        let hits = if self.vectors_ready() {
            self.vectors.read().search(&q, limit, &filter)
        } else {
            self.search_on_disk(&q, limit, &filter)
        };
        let keys: Vec<i64> = hits.iter().map(|(f, c, _, _)| (f << FTS_SHIFT) | *c as i64).collect();
        let texts = self.passages(&keys);
        hits.into_iter()
            .map(|(file_id, chunk, _, sim)| VectorHit {
                file_id,
                chunk: chunk as i64,
                text: texts.get(&((file_id << FTS_SHIFT) | chunk as i64)).cloned().unwrap_or_default(),
                similarity: sim,
            })
            .collect()
    }

    fn search_on_disk(&self, q: &[f32], limit: usize, filter: &Filter) -> Vec<(i64, i32, i8, f32)> {
        let db = self.reader.lock();
        let mut found: Vec<(f32, i64, i8)> = Vec::new();
        let mut vec = Vec::with_capacity(EMBED_DIM);
        let scan = (|| -> rusqlite::Result<()> {
            let mut st = db.prepare("SELECT id, folder_id, kind, vector FROM vectors")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let key: i64 = r.get(0)?;
                let kind = r.get::<_, i64>(2)? as i8;
                if !filter.allows(kind, r.get(1)?, key >> FTS_SHIFT) {
                    continue;
                }
                vec.clear();
                from_blob(r.get_ref(3)?.as_blob()?, &mut vec);
                let n = dot(&vec, &vec).sqrt().max(1e-9);
                found.push((dot(&vec, q) / n, key, kind));
                if found.len() > limit * 4 + 1024 {
                    found.select_nth_unstable_by(limit, |x, y| y.0.total_cmp(&x.0));
                    found.truncate(limit);
                }
            }
            Ok(())
        })();
        if let Err(e) = scan {
            log::warn!("Searching the vectors on disk failed: {e}");
        }
        found.sort_by(|x, y| y.0.total_cmp(&x.0));
        found.truncate(limit);
        found.into_iter().map(|(s, key, kind)| (key >> FTS_SHIFT, (key & CHUNK_MASK) as i32, kind, s)).collect()
    }

    /// Passage texts by keyword-index row id (file_id << 10 | chunk).
    fn passages(&self, keys: &[i64]) -> HashMap<i64, String> {
        let mut out = HashMap::new();
        let db = self.reader.lock();
        for part in keys.chunks(500) {
            let sql = format!("SELECT rowid, text FROM chunks_fts WHERE rowid IN ({})", ids_sql(part));
            if let Ok(mut st) = db.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                    out.extend(rows.flatten());
                }
            }
        }
        out
    }

    /// The terms more than half of the passages hold (the last one also as the start of longer
    /// words, when `prefix`): in a keyword search their weight is next to nothing, and ranking all
    /// their passages takes long.
    pub fn common_terms(&self, terms: &[String], prefix: bool) -> HashSet<String> {
        let version = self.version();
        let db = self.reader.lock();
        let cached = self.passages_cache.lock().filter(|c| c.0 == version).map(|c| c.1);
        let total = match cached {
            Some(n) => n,
            None => {
                let Ok(n) = db.prepare_cached("SELECT count(*) FROM chunks_fts_docsize").and_then(|mut st| st.query_row([], |r| r.get::<_, i64>(0))) else {
                    return HashSet::new();
                };
                *self.passages_cache.lock() = Some((version, n));
                n
            }
        };
        let mut common = HashSet::new();
        for (i, t) in terms.iter().enumerate() {
            let held: rusqlite::Result<Option<i64>> = if prefix && i + 1 == terms.len() {
                // (the most any one word with this start is held: with several, the union is more)
                let next: String = match t.chars().last() {
                    Some(c) => t[..t.len() - c.len_utf8()].to_string() + &char::from_u32(c as u32 + 1).unwrap_or(c).to_string(),
                    None => continue,
                };
                db.prepare_cached("SELECT max(doc) FROM chunks_vocab WHERE term >= ? AND term < ?")
                    .and_then(|mut st| st.query_row(params![t, next], |r| r.get(0)))
            } else {
                db.prepare_cached("SELECT doc FROM chunks_vocab WHERE term = ?").and_then(|mut st| st.query_row([t], |r| r.get(0)).optional())
            };
            if held.ok().flatten().is_some_and(|n| n * 2 > total) {
                common.insert(t.clone());
            }
        }
        common
    }

    /// Full-text matches, best first. `fts` is an FTS5 query string.
    pub fn keyword_search(&self, fts: &str, limit: usize, kinds: &[&str], folder_id: Option<i64>, under: Option<&str>) -> Vec<KeywordHit> {
        let mut wheres = vec!["chunks_fts MATCH ?".to_string()];
        let mut values: Vec<rusqlite::types::Value> = vec![fts.to_string().into()];
        if !kinds.is_empty() {
            wheres.push(format!("f.kind IN ({})", vec!["?"; kinds.len()].join(",")));
            values.extend(kinds.iter().map(|k| rusqlite::types::Value::from(k.to_string())));
        }
        if let Some(f) = folder_id.filter(|&f| f != 0) {
            wheres.push("f.folder_id = ?".into());
            values.push(f.into());
        }
        if let Some(u) = under.filter(|u| !u.is_empty()) {
            let (lo, hi) = inside(u);
            wheres.push("f.path >= ? AND f.path < ?".into());
            values.push(lo.into());
            values.push(hi.into());
        }
        // Rank first, then read the texts of just the best.
        let join = if wheres.len() > 1 { format!("JOIN files f ON f.id = (chunks_fts.rowid >> {FTS_SHIFT})") } else { String::new() };
        let sql = format!(
            "SELECT chunks_fts.rowid AS rid, bm25(chunks_fts, 4.0, 1.0) AS rank FROM chunks_fts {join}
             WHERE {} ORDER BY rank LIMIT ?",
            wheres.join(" AND ")
        );
        values.push((limit as i64).into());
        let db = self.reader.lock();
        let ranked: Vec<i64> = match db.prepare(&sql).and_then(|mut st| {
            st.query_map(params_from_iter(values.iter()), |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<i64>>>()
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(), // e.g. a query FTS5 can't parse
        };
        let mut texts: HashMap<i64, (String, String)> = HashMap::new();
        if !ranked.is_empty() {
            let sql = format!("SELECT rowid, name, text FROM chunks_fts WHERE rowid IN ({})", ids_sql(&ranked));
            if let Ok(mut st) = db.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?)))) {
                    texts.extend(rows.flatten());
                }
            }
        }
        ranked
            .into_iter()
            .filter_map(|rid| {
                let (name, text) = texts.remove(&rid)?;
                Some(KeywordHit { file_id: rid >> FTS_SHIFT, chunk: rid & CHUNK_MASK, name, text })
            })
            .collect()
    }

    /// (file ids, vectors) of these files' passages.
    pub fn vectors_of(&self, file_ids: &[i64]) -> (Vec<i64>, Vec<f32>) {
        if file_ids.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let set: HashSet<i64> = file_ids.iter().copied().collect();
        if self.vectors_ready() {
            return self.vectors.read().rows_of(Some(&set));
        }
        let (mut ids, mut vecs) = (Vec::new(), Vec::new());
        let db = self.reader.lock();
        if let Ok(mut st) = db.prepare("SELECT id, vector FROM vectors WHERE id BETWEEN ? AND ?") {
            for &f in file_ids {
                let (lo, hi) = fts_range(f);
                if let Ok(mut rows) = st.query([lo, hi]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(id), Ok(b)) = (r.get::<_, i64>(0), r.get_ref(1).and_then(|v| Ok(v.as_blob()?.to_vec()))) {
                            ids.push(id >> FTS_SHIFT);
                            from_blob(&b, &mut vecs);
                        }
                    }
                }
            }
        }
        (ids, vecs)
    }

    pub fn file_vectors(&self, file_id: i64) -> Vec<f32> {
        self.vectors_of(&[file_id]).1
    }

    /// Indexed files anywhere inside a folder.
    pub fn file_ids_under(&self, path: &str) -> Vec<i64> {
        let (lo, hi) = inside(path);
        let db = self.reader.lock();
        db.prepare_cached("SELECT id FROM files WHERE status = 'indexed' AND path >= ? AND path < ?")
            .and_then(|mut st| st.query_map(params![lo, hi], |r| r.get(0))?.collect())
            .unwrap_or_default()
    }

    /// The folders directly inside `path` that hold searchable files, with how many.
    pub fn subdirs(&self, path: &str) -> Vec<Value> {
        let (prefix, hi) = inside(path);
        let db = self.reader.lock();
        let rows: Vec<String> = db
            .prepare_cached("SELECT path FROM files WHERE status IN ('indexed', 'pending') AND path >= ? AND path < ?")
            .and_then(|mut st| st.query_map(params![prefix, hi], |r| r.get(0))?.collect())
            .unwrap_or_default();
        drop(db);
        let mut counts: HashMap<&str, i64> = HashMap::new();
        for p in &rows {
            // (a path found starts with the prefix but for ASCII case, which keeps its length)
            if let Some((name, _)) = p.get(prefix.len()..).and_then(|rest| rest.split_once('\\')) {
                *counts.entry(name).or_insert(0) += 1;
            }
        }
        let mut list: Vec<(&str, i64)> = counts.into_iter().collect();
        list.sort_by_cached_key(|(name, _)| name.to_lowercase());
        list.into_iter()
            .map(|(name, count)| json!({"path": format!("{prefix}{name}"), "name": name, "count": count, "kind": dir_kind(name)}))
            .collect()
    }

    /// Folders anywhere in the indexed ones whose name holds `text` (any case): the best `limit`,
    /// names that start with it first, then by how many files they hold.
    pub fn find_dirs(&self, text: &str, limit: usize) -> Vec<Value> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let roots: Vec<String> = self.folders().into_iter().map(|f| f.path.trim_end_matches(['\\', '/']).to_string()).collect();
        let db = self.reader.lock();
        let rows: Vec<String> = db
            .prepare_cached("SELECT path FROM files WHERE status IN ('indexed', 'pending')")
            .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        drop(db);
        let mut counts: HashMap<String, i64> = HashMap::new();
        for p in &rows {
            let under = |r: &&String| p.get(..r.len()).is_some_and(|h| h.eq_ignore_ascii_case(r)) && p.as_bytes().get(r.len()) == Some(&b'\\');
            let Some(root) = roots.iter().find(under) else { continue };
            // Each folder between the root and the file (not the root, not the file).
            let rest = &p[root.len()..];
            let mut end = 0;
            for (i, c) in rest.char_indices().skip(1) {
                if c == '\\' {
                    let dir = &rest[end + 1..i];
                    if dir.to_lowercase().contains(&needle) {
                        *counts.entry(format!("{root}{}", &rest[..i])).or_insert(0) += 1;
                    }
                    end = i;
                }
            }
        }
        let mut list: Vec<(String, i64)> = counts.into_iter().collect();
        let starts = |path: &str| !split_last(path).to_lowercase().starts_with(&needle);
        list.sort_by(|a, b| starts(&a.0).cmp(&starts(&b.0)).then(b.1.cmp(&a.1)).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        list.into_iter()
            .take(limit)
            .map(|(path, count)| {
                let name = split_last(&path).to_string();
                json!({"path": path, "kind": dir_kind(&name), "name": name, "count": count})
            })
            .collect()
    }

    /// Each file's best cosine similarity to `vector`, over its chunks.
    pub fn similarities(&self, vector: &[f32], file_ids: &[i64]) -> HashMap<i64, f32> {
        let (ids, vecs) = self.vectors_of(file_ids);
        let q = normalized(vector);
        let mut best: HashMap<i64, f32> = HashMap::new();
        for (i, fid) in ids.iter().enumerate() {
            let v = &vecs[i * EMBED_DIM..(i + 1) * EMBED_DIM];
            let s = dot(v, &q) / dot(v, v).sqrt().max(1e-9);
            let e = best.entry(*fid).or_insert(-2.0);
            if s > *e {
                *e = s;
            }
        }
        best
    }

    #[cfg(test)]
    pub fn vector_count(&self) -> i64 {
        let db = self.db.lock();
        db.query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0)).unwrap_or(0)
    }
}

/// User settings, kept in the meta table.
pub struct Settings {
    store: Arc<Store>,
    values: Mutex<Map<String, Value>>,
}

pub fn defaults() -> Map<String, Value> {
    // Updates are looked for by themselves; installed when you say so, or by themselves if you want.
    let v = json!({
        "problems_seen": 0, "auto_update": false, "seen_version": "",
        "language": "auto", "theme": "auto", "view_files": "cards", "view_images": "grid",
        // File types (see filetypes.rs): the kinds and the formats switched off, and the rules
        "kinds_off": [], "formats_off": [], "hidden_files": false, "program_folders": false,
        // Settings -> Diagnostics: a tab with what the app uses
        "diagnostics": false,
        // The map's side column, as wide as it was dragged to (pixels; 0: its own width)
        "map_side": 0,
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

impl Settings {
    pub fn new(store: Arc<Store>) -> Settings {
        let mut values = defaults();
        for (key, value) in values.iter_mut() {
            if let Some(raw) = store.meta_get(&format!("setting.{key}")) {
                if let Ok(v) = serde_json::from_str(&raw) {
                    *value = v;
                }
            }
        }
        Settings { store, values: Mutex::new(values) }
    }

    pub fn get(&self, key: &str) -> Value {
        self.values.lock().get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn str(&self, key: &str) -> String {
        self.get(key).as_str().unwrap_or_default().to_string()
    }

    pub fn all(&self) -> Map<String, Value> {
        self.values.lock().clone()
    }

    pub fn update(&self, key: &str, value: Value) -> Result<()> {
        let mut values = self.values.lock();
        if !values.contains_key(key) {
            anyhow::bail!("unknown setting {key}");
        }
        self.store.meta_set(&format!("setting.{key}"), &serde_json::to_string(&value)?);
        values.insert(key.to_string(), value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Arc<Store>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("quiekel-rs-store-{}-{}", std::process::id(), rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        (Arc::new(Store::open_at(&dir.join("state.db")).unwrap()), dir)
    }

    fn rand_tag() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; EMBED_DIM];
        v[i] = 1.0;
        v
    }

    #[test]
    fn save_search_and_remove() {
        let (s, dir) = store();
        let folder = s.add_folder(r"C:\docs").unwrap();
        s.add_pending(folder, &[(r"C:\docs\a.txt".into(), 10, 1.0, "text".into(), "a.txt".into())]).unwrap();
        assert_eq!(s.folders()[0].pending, 1);
        let entry = |p: &str| Entry {
            folder_id: folder, path: p.into(), size: 10, mtime: 1.0, kind: "text".into(), status: "indexed".into(),
            chunks: 1, error: None, search_name: "rent contract".into(),
        };
        let row = |p: &str, i: usize, text: &str| VectorRow {
            path: p.into(), folder_id: folder, chunk: 0, kind: "text".into(), text: text.into(), vector: unit(i), page: None,
        };
        s.save_files(&[entry(r"C:\docs\a.txt"), entry(r"C:\docs\b.txt")],
                     &[row(r"C:\docs\a.txt", 0, "rent is due"), row(r"C:\docs\b.txt", 1, "holiday photos")]).unwrap();
        s.keep_vectors.store(true, Ordering::SeqCst);
        let hits = s.search(&unit(1), 10, &[], None, None);
        assert_eq!(hits[0].text, "holiday photos");
        assert!((hits[0].similarity - 1.0).abs() < 1e-6);
        let kw = s.keyword_search("\"rent\"", 10, &["text"], Some(folder), Some(r"C:\docs"));
        assert_eq!(kw.len(), 2); // the passage, and the names of both files
        // On disk gives the same as in memory.
        *s.vectors.write() = Vectors::default();
        s.keep_vectors.store(false, Ordering::SeqCst);
        assert_eq!(s.search(&unit(1), 10, &[], None, None)[0].text, "holiday photos");
        s.remove_paths(&[r"C:\docs\b.txt".into()]).unwrap();
        assert_eq!(s.vector_count(), 1);
        assert_eq!(s.subdirs(r"C:\")[0]["name"], "docs");
        let settings = Settings::new(s.clone());
        settings.update("theme", json!("dark")).unwrap();
        assert_eq!(Settings::new(s.clone()).get("theme"), json!("dark"));
        assert!(settings.update("nope", json!(1)).is_err());
        drop(settings);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    use std::collections::BTreeSet;

    fn save(s: &Store, folder_id: i64, path: &str, kind: &str, texts: &[&str], name: &str) {
        let entry = Entry {
            folder_id, path: path.into(), size: 1, mtime: 1.0, kind: kind.into(), status: "indexed".into(),
            chunks: texts.len() as i64, error: None, search_name: name.into(),
        };
        let rows: Vec<VectorRow> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| VectorRow {
                path: path.into(), folder_id, chunk: i as i64, kind: kind.into(), text: t.to_string(), vector: vec![1.0; EMBED_DIM], page: None,
            })
            .collect();
        s.save_files(&[entry], &rows).unwrap();
    }

    /// Unit vectors that look random (and are the same every run).
    struct Rng(u64);

    impl Rng {
        fn unit(&mut self) -> Vec<f32> {
            let mut v: Vec<f32> = (0..EMBED_DIM)
                .map(|_| {
                    self.0 ^= self.0 << 13;
                    self.0 ^= self.0 >> 7;
                    self.0 ^= self.0 << 17;
                    (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
                })
                .collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            v
        }
    }

    fn save_vectors(s: &Store, folder_id: i64, path: &str, kind: &str, vectors: Vec<Vec<f32>>) {
        let entry = Entry {
            folder_id, path: path.into(), size: 1, mtime: 1.0, kind: kind.into(), status: "indexed".into(),
            chunks: vectors.len() as i64, error: None, search_name: path.rsplit('\\').next().unwrap().into(),
        };
        let rows: Vec<VectorRow> = vectors
            .into_iter()
            .enumerate()
            .map(|(i, v)| VectorRow { path: path.into(), folder_id, chunk: i as i64, kind: kind.into(), text: format!("{path} #{i}"), vector: v, page: None })
            .collect();
        s.save_files(&[entry], &rows).unwrap();
    }

    #[test]
    fn the_keyword_index_follows_adds_updates_and_removals() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\data").unwrap();
        save(&s, fid, r"C:\data\a.txt", "text", &["electricity bill", "second part"], "a.txt");
        save(&s, fid, r"C:\data\IMG_0042.jpg", "image", &[""], "IMG_0042.jpg photos");
        let find = |q: &str, kinds: &[&str]| s.keyword_search(q, 10, kinds, None, None);
        assert_eq!(find("\"electricity\"", &[]).iter().map(|h| h.chunk).collect::<Vec<_>>(), [0]);
        assert!(find("\"0042\"", &["image"])[0].name.starts_with("IMG_0042"));
        assert!(find("\"0042\"", &["text"]).is_empty());

        save(&s, fid, r"C:\data\a.txt", "text", &["water bill"], "a.txt"); // the file changed
        assert!(find("\"electricity\"", &[]).is_empty());
        assert_eq!(find("\"water\"", &[]).len(), 1);

        s.remove_paths(&[r"C:\data\a.txt".into()]).unwrap();
        assert!(find("\"water\"", &[]).is_empty());
        s.remove_folder(fid).unwrap();
        assert!(find("\"0042\"", &[]).is_empty());
        assert_eq!(s.vector_count(), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn searching_inside_one_folder() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\data").unwrap();
        save(&s, fid, r"C:\data\a\x.txt", "text", &["apple pie"], "x.txt a");
        save(&s, fid, r"C:\data\a\b\y.txt", "text", &["apple juice"], "y.txt a b");
        save(&s, fid, r"C:\data\ab\w.txt", "text", &["apple cake"], "w.txt ab");
        save(&s, fid, r"C:\data\c\z.txt", "text", &["apple tree"], "z.txt c");

        assert_eq!(s.file_ids_under(r"C:\data\a").len(), 2); // not the files in "ab"
        let subdirs = |p: &str| -> Vec<(String, i64)> {
            s.subdirs(p).iter().map(|d| (d["name"].as_str().unwrap().to_string(), d["count"].as_i64().unwrap())).collect()
        };
        assert_eq!(subdirs(r"C:\data"), [("a".to_string(), 2), ("ab".to_string(), 1), ("c".to_string(), 1)]);
        assert_eq!(subdirs(r"C:\data\a").into_iter().map(|d| d.0).collect::<Vec<_>>(), ["b"]);
        assert_eq!(s.keyword_search("\"apple\"", 10, &[], None, Some(r"C:\DATA\a\\")).len(), 2); // case and a trailing \ don't matter

        let sims = s.similarities(&vec![1.0; EMBED_DIM], &s.file_ids_under(r"C:\data"));
        assert!(sims.len() == 4 && sims.values().all(|v| (v - 1.0).abs() < 1e-5));
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_persist() {
        let (s, dir) = store();
        let settings = Settings::new(s.clone());
        assert_eq!(settings.get("theme"), json!("auto"));
        settings.update("theme", json!("dark")).unwrap();
        settings.update("view_files", json!("list")).unwrap();
        let again = Settings::new(s.clone());
        assert!(again.get("theme") == json!("dark") && again.get("view_files") == json!("list"));
        drop((settings, again, s));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_in_memory_finds_the_same_as_on_disk() {
        let (s, dir) = store();
        let mut rng = Rng(3);
        let (a, b) = (s.add_folder(r"C:\a").unwrap(), s.add_folder(r"C:\b").unwrap());
        let mut images = HashSet::new();
        for i in 0..40 {
            let kind = if i % 4 == 0 { "image" } else { "text" };
            let (fid, place) = if i % 2 == 1 { (a, "a") } else { (b, "b") };
            let path = format!(r"C:\{place}\f{i}.x");
            save_vectors(&s, fid, &path, kind, (0..if kind == "image" { 1 } else { 3 }).map(|_| rng.unit()).collect());
            if kind == "image" {
                images.insert(s.signature(&path).unwrap().0);
            }
        }
        let q = rng.unit();
        let some: Vec<i64> = s.file_ids_under(r"C:\a").into_iter().take(7).collect();
        let searches = |s: &Store| {
            vec![
                s.search(&q, 12, &[], None, None),
                s.search(&q, 12, &["image"], None, None),
                s.search(&q, 12, &[], Some(a), None),
                s.search(&q, 12, &[], None, Some(&some)),
                s.search(&q, 12, &["text"], Some(b), None),
            ]
        };
        s.keep_vectors.store(true, Ordering::SeqCst);
        let in_memory = searches(&s);
        assert!(s.vectors.read().ready);
        s.keep_vectors_in_memory(false);
        let on_disk = searches(&s);
        assert!(!s.vectors.read().ready);
        let key = |hits: &Vec<VectorHit>| hits.iter().map(|h| (h.file_id, h.chunk, h.text.clone())).collect::<Vec<_>>();
        for (mem, disk) in in_memory.iter().zip(&on_disk) {
            assert_eq!(key(mem), key(disk));
            assert!(mem.iter().zip(disk).all(|(m, d)| (m.similarity - d.similarity).abs() < 2e-3)); // (half precision in memory)
        }
        assert!(in_memory[1].len() == 10 && in_memory[1].iter().all(|h| images.contains(&h.file_id)));
        assert!(in_memory[3].iter().all(|h| some.contains(&h.file_id)));
        assert!(s.search(&q, 5, &[], None, Some(&[])).is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn vectors_in_memory_follow_every_write() {
        let (s, dir) = store();
        let mut rng = Rng(4);
        let fid = s.add_folder(r"C:\a").unwrap();
        for i in 0..10 {
            save_vectors(&s, fid, &format!(r"C:\a\f{i}.x"), "text", vec![rng.unit(), rng.unit()]);
        }
        s.keep_vectors.store(true, Ordering::SeqCst);
        s.search(&rng.unit(), 1, &[], None, None); // (fills them on first use)
        assert!(s.vectors.read().ready);
        let target = rng.unit();
        save_vectors(&s, fid, r"C:\a\f3.x", "text", vec![target.clone()]); // f3 changed: now one passage, the target
        let top = s.search(&target, 3, &[], None, None);
        let f3 = s.signature(r"C:\a\f3.x").unwrap().0;
        assert!(top[0].file_id == f3 && 1.0 - top[0].similarity < 1e-5);
        assert_eq!(top.iter().filter(|h| h.file_id == f3).map(|h| h.chunk).collect::<Vec<_>>(), [0]); // its old passages are gone
        assert_eq!(s.file_vectors(f3).len(), EMBED_DIM); // one passage
        s.remove_paths(&[r"C:\a\f3.x".into()]).unwrap();
        assert!(s.search(&target, 20, &[], None, None).iter().all(|h| h.file_id != f3));
        s.remove_folder(fid).unwrap();
        assert!(s.search(&target, 20, &[], None, None).is_empty() && s.vectors.read().rows == 0);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_model_or_format_starts_the_index_over() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\a").unwrap();
        save_vectors(&s, fid, r"C:\a\f.x", "text", vec![Rng(7).unit(), Rng(8).unit()]);
        assert_eq!(s.vector_count(), 2);
        s.meta_set("index", "someone/another-model@256/f2"); // (made with another model)
        std::fs::write(s.map_path(), b"QEMAP001").unwrap();
        std::fs::create_dir_all(dir.join("vectors").join("chunks.lance")).unwrap(); // (versions before 1.0)
        std::fs::write(dir.join("map.npz"), b"old").unwrap();
        drop(s);
        let again = Store::open_at(&dir.join("state.db")).unwrap();
        assert!(again.vector_count() == 0 && again.signature(r"C:\a\f.x").is_none());
        assert!(again.folder(fid).is_some()); // the folders stay
        assert!(!again.map_path().exists()); // the map of the files that were
        assert!(!dir.join("vectors").exists() && !dir.join("map.npz").exists()); // and what older versions kept
        drop(again);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_files_warn_once() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\a").unwrap();
        let unreadable = |path: &str| {
            let e = Entry {
                folder_id: fid, path: path.into(), size: 1, mtime: 1.0, kind: "image".into(), status: "error".into(), chunks: 0,
                error: Some("!damaged_image".into()), search_name: String::new(),
            };
            s.save_files(&[e], &[]).unwrap();
        };
        let warnings = || {
            let f = &s.folders()[0];
            (f.errors, f.unreadable)
        };
        let unseen = || -> BTreeSet<String> {
            s.errors(fid, 100).iter().filter(|e| e["seen"] == json!(false)).map(|e| e["path"].as_str().unwrap().to_string()).collect()
        };
        unreadable(r"C:\a\x.jpg");
        unreadable(r"C:\a\y.jpg");
        assert_eq!(warnings(), (2, 2));
        s.errors_seen(fid); // looked at in the folder's details
        assert_eq!(warnings(), (0, 2));
        let listed = s.errors(fid, 100);
        assert!(listed.len() == 2 && listed.iter().all(|e| e["seen"] == json!(true))); // still listed
        unreadable(r"C:\a\x.jpg"); // tried again, still broken: no new warning
        unreadable(r"C:\a\z.jpg"); // a new one warns
        assert_eq!(unseen(), BTreeSet::from([r"C:\a\z.jpg".to_string()]));
        save_vectors(&s, fid, r"C:\a\y.jpg", "image", vec![Rng(1).unit()]); // fixed
        unreadable(r"C:\a\y.jpg"); // broken again later: that's news, so it warns
        assert_eq!(unseen(), BTreeSet::from([r"C:\a\y.jpg".to_string(), r"C:\a\z.jpg".to_string()]));
        assert_eq!(s.folders()[0].errors, 2);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn whats_inside_a_folder_whatever_the_case_and_not_its_neighbours() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\").unwrap();
        for p in [r"C:\Docs\a.txt", r"C:\docs\Sub\b.txt", r"C:\DOCS\Sub\c.txt", r"C:\Docs2\d.txt", r"C:\Docs]\e.txt", r"C:\Docs\f.zip\in\g.txt"] {
            save(&s, fid, p, "text", &["words"], "x");
        }
        let names = |ids: Vec<i64>| -> BTreeSet<String> {
            s.files_by_ids(&ids).into_values().map(|f| f.path.rsplit('\\').next().unwrap().to_string()).collect()
        };
        let set = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(names(s.file_ids_under(r"c:\docs")), set(&["a.txt", "b.txt", "c.txt", "g.txt"]));
        assert_eq!(names(s.file_ids_under(r"C:\Docs\")), set(&["a.txt", "b.txt", "c.txt", "g.txt"]));
        let dirs: Vec<(String, i64)> = s.subdirs(r"C:\Docs").iter().map(|d| (d["name"].as_str().unwrap().to_string(), d["count"].as_i64().unwrap())).collect();
        assert_eq!(dirs, [("f.zip".to_string(), 1), ("Sub".to_string(), 2)]);
        assert_eq!(s.keyword_search("\"words\"", 10, &[], None, Some(r"C:\DOCS")).len(), 4);
        assert_eq!(s.files_inside(r"C:\Docs\f.zip").len(), 1);
        s.remove_paths(&[r"C:\docs\sub".into()]).unwrap(); // a folder deleted: what was in it goes
        assert_eq!(names(s.file_ids_under(r"C:\")), set(&["a.txt", "d.txt", "e.txt", "g.txt"]));
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_removed_folder_is_gone_at_once_and_its_files_follow_in_steps_also_after_a_restart() {
        let (s, dir) = store();
        let (fa, fb) = (s.add_folder(r"C:\a").unwrap(), s.add_folder(r"C:\b").unwrap());
        let mut rng = Rng(5);
        for i in 0..5 {
            save_vectors(&s, fa, &format!(r"C:\a\f{i}.x"), "text", vec![rng.unit(), rng.unit()]);
        }
        save_vectors(&s, fb, r"C:\b\g.x", "text", vec![rng.unit()]);
        s.keep_vectors.store(true, Ordering::SeqCst);
        s.load_vectors();
        let target = rng.unit();
        assert_eq!(s.search(&target, 20, &[], None, None).len(), 11);
        let a_ids: Vec<i64> = (0..5).map(|i| s.signature(&format!(r"C:\a\f{i}.x")).unwrap().0).collect();
        s.begin_removing(fa);
        // Gone for the page: the folder, its files in searches and on the map.
        assert!(s.folders().iter().all(|f| f.id != fa) && s.folder(fa).is_none() && s.folder(fb).is_some());
        assert!(s.search(&target, 20, &[], None, None).iter().all(|h| !a_ids.contains(&h.file_id)));
        assert!(s.files_by_ids(&a_ids).is_empty());
        assert_eq!(s.map_file_count(), 1);
        assert_eq!(s.map_files().len(), 1);
        // A step, then a restart: it goes on where it stopped.
        assert!(!s.remove_folder_step(fa, 2).unwrap());
        drop(s);
        let s = Store::open_at(&dir.join("state.db")).unwrap();
        assert_eq!(s.removing(), vec![fa]);
        assert!(s.folders().iter().all(|f| f.id != fa));
        s.keep_vectors.store(true, Ordering::SeqCst);
        s.load_vectors(); // (not the vectors of a folder on its way out)
        assert_eq!(s.vectors.read().rows, 1);
        while !s.remove_folder_step(fa, 2).unwrap() {}
        assert!(s.removing().is_empty() && a_ids.iter().all(|&id| s.file(id).is_none()));
        assert_eq!(s.vector_count(), 1);
        assert_eq!(s.keyword_search("\"f0\"", 10, &[], None, None).len(), 0);
        // Its place can be taken again (here while another removal is under way there).
        let again = s.add_folder(r"C:\a").unwrap();
        s.begin_removing(again);
        assert!(s.add_folder(r"C:\a\inner").is_ok());
        assert!(s.removing().is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn half_precision_gives_the_same_scores_and_the_same_best() {
        let mut rng = Rng(9);
        let q = rng.unit();
        let rows: Vec<Vec<f32>> = (0..300).map(|_| rng.unit()).collect();
        for r in &rows {
            let h: Vec<f16> = r.iter().map(|&x| f16::from_f32(x)).collect();
            let mut back = vec![0f32; EMBED_DIM];
            h.convert_to_f32_slice(&mut back);
            assert!((dot16(&h, &q) - dot(&back, &q)).abs() < 1e-5); // (the processor's instructions, or not)
            assert!((dot16(&h, &q) - dot(r, &q)).abs() < 2e-3);
        }
        let mut best = Best::new(10);
        for (i, r) in rows.iter().enumerate() {
            best.add((dot(r, &q), 0, i));
        }
        let mut all: Vec<(f32, usize)> = rows.iter().enumerate().map(|(i, r)| (dot(r, &q), i)).collect();
        all.sort_by(|x, y| y.0.total_cmp(&x.0));
        assert_eq!(best.into_sorted().iter().map(|x| x.2).collect::<Vec<_>>(), all[..10].iter().map(|x| x.1).collect::<Vec<_>>());
    }

    #[test]
    fn words_most_passages_hold_are_common() {
        let (s, dir) = store();
        let fid = s.add_folder("C:/c").unwrap();
        for i in 0..6 {
            save(&s, fid, &format!("C:/c/n{i}.txt"), "text", &[if i == 0 { "rent and holiday" } else { "rent again" }], "n.txt");
        }
        let terms = |w: &[&str]| w.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(s.common_terms(&terms(&["rent", "holiday"]), false), HashSet::from(["rent".to_string()]));
        assert_eq!(s.common_terms(&terms(&["holiday", "ren"]), true), HashSet::from(["ren".to_string()])); // (as the start of "rent")
        assert!(s.common_terms(&terms(&["holi"]), true).is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn folders_are_found_by_their_name_anywhere() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\docs").unwrap();
        for p in [r"C:\docs\Urlaub 2024\a.jpg", r"C:\docs\Urlaub 2024\b.jpg", r"C:\docs\Fotos\urlaub\c.jpg",
                  r"C:\docs\Bergurlaub.zip\d.jpg", r"C:\docs\Arbeit\e.txt", r"C:\docs\urlaub.txt"] {
            save(&s, fid, p, "text", &["x"], "n");
        }
        let found = s.find_dirs("URLAUB", 10);
        let names: Vec<(&str, i64)> = found.iter().map(|d| (d["name"].as_str().unwrap(), d["count"].as_i64().unwrap())).collect();
        // Names that start with it first, then by size; files and the indexed folder itself aren't folders.
        assert_eq!(names, [("Urlaub 2024", 2), ("urlaub", 1), ("Bergurlaub.zip", 1)]);
        assert_eq!(found[1]["path"], r"C:\docs\Fotos\urlaub");
        assert_eq!((found[0]["kind"].clone(), found[2]["kind"].clone()), (json!(null), json!("archive")));
        assert!(s.find_dirs("  ", 10).is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pdfs_indexed_before_pages_get_them_once() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\docs").unwrap();
        save(&s, fid, r"C:\docs\old.pdf", "doc", &["one", "two", "three"], "old.pdf");
        save(&s, fid, r"C:\docs\notes.txt", "text", &["text"], "notes.txt"); // (not a PDF)
        let todo = s.pdfs_without_pages(10);
        assert_eq!(todo.iter().map(|t| (t.1.as_str(), t.2)).collect::<Vec<_>>(), [(r"C:\docs\old.pdf", 3)]);
        let id = todo[0].0;
        s.set_pages(id, &[1, 1, 2]).unwrap();
        assert!(s.pdfs_without_pages(10).is_empty());
        let key = |chunk: i64| (id << FTS_SHIFT) | chunk;
        let pages = s.pages(&[key(0), key(1), key(2)]);
        assert_eq!((pages[&key(0)], pages[&key(2)]), (1, 2));
        // Not known (0): not shown, and not asked for again.
        s.set_pages(id, &[0, 0, 0]).unwrap();
        assert!(s.pages(&[key(0)]).is_empty() && s.pdfs_without_pages(10).is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn documents_whose_text_now_comes_out_better_are_read_again_once() {
        let (s, dir) = store();
        let fid = s.add_folder(r"C:\docs").unwrap();
        save(&s, fid, r"C:\docs\split.pdf", "doc", &["intro", "wird ge\u{2}worfen"], "split.pdf");
        save(&s, fid, r"C:\docs\bullets.pdf", "doc", &["\u{f0b7}\u{f0b7} ."], "bullets.pdf");
        save(&s, fid, r"C:\docs\fine.pdf", "doc", &["wird geworfen"], "fine.pdf");
        save(&s, fid, r"C:\docs\notes.txt", "text", &["a\u{2}b"], "notes.txt"); // (its reader didn't change)
        save(&s, fid, r"C:\docs\a.zip\inner\guide.pdf", "doc", &["\u{f}Punkt"], "guide.pdf");
        let zip = ContainerRow { path: r"C:\docs\a.zip".into(), size: 1, mtime: 5.0, types: String::new(), report: Default::default() };
        let other = ContainerRow { path: r"C:\docs\b.zip".into(), size: 1, mtime: 5.0, types: String::new(), report: Default::default() };
        s.save_containers(fid, &[zip, other], false).unwrap();
        assert_eq!(s.reread_documents().unwrap(), 3);
        let mtime = |p: &str| s.signature(p).unwrap().2;
        for p in [r"C:\docs\split.pdf", r"C:\docs\bullets.pdf", r"C:\docs\a.zip\inner\guide.pdf"] {
            assert_eq!(mtime(p), 0.0, "{p}");
        }
        assert_eq!((mtime(r"C:\docs\fine.pdf"), mtime(r"C:\docs\notes.txt")), (1.0, 1.0));
        let containers = s.containers(fid);
        assert_eq!((containers[r"c:\docs\a.zip"].mtime, containers[r"c:\docs\b.zip"].mtime), (0.0, 5.0));
        assert_eq!(s.reread_documents().unwrap(), 0); // (once)
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
