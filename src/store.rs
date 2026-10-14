//! Persistent state, in one SQLite file: folders, files, settings, keyword search (FTS5) and the
//! passages' vectors.
//!
//! Searches don't scan the vectors on disk: every passage's vector is also kept in memory
//! (`Vectors`, about 1 KB per passage), which turns a search into a few milliseconds of maths.
//! Light performance turns that off; searches then read the vectors from disk.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::config::{self, EMBED_DIM};
use crate::filetypes::Report;
use crate::search::{KeywordHit, VectorHit};

/// Bump when what goes into the index changes (e.g. how titles are built): forces a re-index.
pub const INDEX_FORMAT: i64 = 3;

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
CREATE INDEX IF NOT EXISTS files_folder ON files(folder_id);
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
-- The passages' vectors (EMBED_DIM float32 each), id as in chunks_fts.
CREATE TABLE IF NOT EXISTS vectors (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    vector BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS vectors_folder ON vectors(folder_id);
";

pub const FTS_SHIFT: i64 = 10; // up to 1024 chunks per file in the keyword index
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

fn fts_range(file_id: i64) -> (i64, i64) {
    (file_id << FTS_SHIFT, (file_id << FTS_SHIFT) | CHUNK_MASK)
}

/// 'C:\x' -> 'C:\x\', for matching the paths of everything inside a folder.
fn dir_prefix(path: &str) -> String {
    format!("{}\\", path.trim_end_matches(['\\', '/']))
}

/// Windows' case-insensitive paths, as Python's os.path.normcase.
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
    pub unsearched: i64,
}

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
}

// ---- vectors in memory -------------------------------------------------------------------------

/// Rows of vectors added together (a save, or everything at start-up).
pub struct Block {
    ids: Vec<i64>,
    chunks: Vec<i32>,
    kinds: Vec<i8>,
    folders: Vec<i64>,
    vecs: Vec<f32>,
    alive: Vec<bool>,
}

impl Block {
    fn new(ids: Vec<i64>, chunks: Vec<i32>, kinds: Vec<i8>, folders: Vec<i64>, vecs: Vec<f32>) -> Block {
        let alive = vec![true; ids.len()];
        Block { ids, chunks, kinds, folders, vecs, alive }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn row(&self, i: usize) -> &[f32] {
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
    pub const MAX_ROWS: usize = 500_000; // ~500 MB: bigger indexes are searched on disk instead
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
        let mut found: Vec<(f32, usize, usize)> = Vec::new();
        for (bi, b) in self.blocks.iter().enumerate() {
            let mut sims: Vec<(f32, usize, usize)> = (0..b.len())
                .filter(|&i| b.alive[i] && filter.allows(b.kinds[i], b.folders[i], b.ids[i]))
                .map(|i| (dot(b.row(i), q), bi, i))
                .collect();
            let k = limit.min(sims.len());
            if k == 0 {
                continue;
            }
            if k < sims.len() {
                sims.select_nth_unstable_by(k - 1, |x, y| y.0.total_cmp(&x.0));
                sims.truncate(k);
            }
            found.extend(sims);
        }
        found.sort_by(|x, y| y.0.total_cmp(&x.0));
        found.truncate(limit);
        found
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
                    vecs.extend_from_slice(b.row(i));
                }
            }
        }
        (ids, vecs)
    }
}

// ---- the store -----------------------------------------------------------------------------------

pub struct Store {
    db: Mutex<Connection>,
    dir: std::path::PathBuf, // the folder the index is in (the map's file goes next to it)
    pub vectors: RwLock<Vectors>,
    version: AtomicU64,
    folders_cache: Mutex<Option<(u64, Vec<Folder>)>>,
    keep_vectors: AtomicBool,
    too_many: AtomicBool, // more passages than fit in memory: search them on disk
    loading: Mutex<()>,
}

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
        db.pragma_update(None, "journal_mode", "WAL")?;
        // In WAL mode this is still crash-safe; a power cut may only undo the last saves, which
        // are then indexed again.
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.execute_batch(SCHEMA)?;
        let store = Store {
            db: Mutex::new(db),
            dir: path.parent().map(std::path::Path::to_path_buf).unwrap_or_default(),
            vectors: RwLock::new(Vectors::default()),
            version: AtomicU64::new(0),
            folders_cache: Mutex::new(None),
            keep_vectors: AtomicBool::new(true),
            too_many: AtomicBool::new(false),
            loading: Mutex::new(()),
        };
        store.check_index_format()?;
        Ok(store)
    }

    fn changed(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
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
        }
        self.meta_set("index", &current);
        Ok(())
    }

    // ---- vectors in memory ------------------------------------------------------------------

    /// Balanced and Maximum performance keep them (and fill them now, in the background); Light
    /// lets them go and searches the vectors on disk.
    pub fn keep_vectors_in_memory(self: &Arc<Self>, keep: bool) {
        self.keep_vectors.store(keep, Ordering::SeqCst);
        if keep {
            let me = self.clone();
            std::thread::Builder::new().name("vectors".into()).spawn(move || me.load_vectors()).ok();
        } else {
            *self.vectors.write() = Vectors::default();
        }
    }

    /// Whether the search data is kept in memory (Balanced and Maximum) or searched on disk.
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
        let read = (|| -> rusqlite::Result<()> {
            let mut st = db.prepare("SELECT id, folder_id, kind, vector FROM vectors")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                ids.push(id >> FTS_SHIFT);
                chunks.push((id & CHUNK_MASK) as i32);
                folders.push(r.get(1)?);
                kinds.push(r.get::<_, i64>(2)? as i8);
                from_blob(r.get_ref(3)?.as_blob()?, &mut vecs);
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
        if v.ready { (v.rows * EMBED_DIM * 4) as f64 / 1e6 } else { 0.0 }
    }

    // ---- meta & settings ------------------------------------------------------------------

    pub fn meta_get(&self, key: &str) -> Option<String> {
        let db = self.db.lock();
        db.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0)).optional().ok().flatten()
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
        let version = self.version();
        if let Some((v, f)) = self.folders_cache.lock().as_ref() {
            if *v == version {
                return f.clone();
            }
        }
        let db = self.db.lock();
        let result = (|| -> rusqlite::Result<Vec<Folder>> {
            let mut st = db.prepare(
                "SELECT f.id, f.path, f.watch, f.added_at, f.last_scan_at, f.report,
                       COUNT(x.id) AS files,
                       COALESCE(SUM(x.status = 'indexed'), 0) AS indexed,
                       COALESCE(SUM(x.status = 'pending'), 0) AS pending,
                       COALESCE(SUM(x.status = 'error' AND x.muted = 0), 0) AS errors,
                       COALESCE(SUM(x.status = 'error'), 0) AS unreadable,
                       COALESCE(SUM(x.status = 'skipped'), 0) AS skipped,
                       COALESCE(SUM(CASE WHEN x.status = 'indexed' THEN x.size ELSE 0 END), 0) AS bytes,
                       COALESCE(SUM(x.chunks), 0) AS chunks,
                       COALESCE(SUM(x.kind = 'image' AND x.status = 'indexed'), 0) AS images
                FROM folders f LEFT JOIN files x ON x.folder_id = f.id
                GROUP BY f.id ORDER BY f.path",
            )?;
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
                    // Everything that isn't searched: skipped files, and what the scan left out.
                    unsearched: skipped + report.total(),
                })
            })?;
            rows.collect()
        })()
        .unwrap_or_default();
        *self.folders_cache.lock() = Some((version, result.clone()));
        result
    }

    pub fn folder(&self, folder_id: i64) -> Option<FolderRow> {
        let db = self.db.lock();
        db.query_row("SELECT id, path, watch FROM folders WHERE id = ?", [folder_id], |r| {
            Ok(FolderRow { path: r.get(1)? })
        })
        .optional()
        .ok()
        .flatten()
    }

    pub fn add_folder(&self, path: &str) -> Result<i64> {
        let db = self.db.lock();
        db.execute("INSERT INTO folders (path, added_at) VALUES (?, ?)", params![path, now()])?;
        let id = db.last_insert_rowid();
        drop(db);
        self.changed();
        Ok(id)
    }

    pub fn remove_folder(&self, folder_id: i64) -> Result<()> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        let ids: Vec<i64> = {
            let mut st = tx.prepare("SELECT id FROM files WHERE folder_id = ?")?;
            st.query_map([folder_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        Self::delete_fts(&tx, &ids)?;
        tx.execute("DELETE FROM vectors WHERE folder_id = ?", [folder_id])?;
        tx.execute("DELETE FROM files WHERE folder_id = ?", [folder_id])?;
        tx.execute("DELETE FROM containers WHERE folder_id = ?", [folder_id])?;
        tx.execute("DELETE FROM folders WHERE id = ?", [folder_id])?;
        self.vectors.write().drop_rows(&|_, folder| folder == folder_id);
        tx.commit()?;
        self.too_many.store(false, Ordering::SeqCst);
        drop(db);
        self.changed();
        Ok(())
    }

    pub fn set_watch(&self, folder_id: i64, watch: bool) {
        let db = self.db.lock();
        let _ = db.execute("UPDATE folders SET watch = ? WHERE id = ?", params![watch as i64, folder_id]);
        drop(db);
        self.changed();
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
        self.changed();
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
        let db = self.db.lock();
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
        let db = self.db.lock();
        db.query_row(&format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?"), [file_id], FileRecord::from_row)
            .optional()
            .ok()
            .flatten()
    }

    /// The start of a file's text, as shown in search results ("" for pictures).
    pub fn first_passage(&self, file_id: i64) -> String {
        let db = self.db.lock();
        db.query_row("SELECT text FROM chunks_fts WHERE rowid = ?", [file_id << FTS_SHIFT], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn files_by_ids(&self, ids: &[i64]) -> HashMap<i64, FileRecord> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return out;
        }
        let db = self.db.lock();
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
        out
    }

    /// Every file with vectors, by id: (id, mtime, size).
    pub fn map_files(&self) -> Vec<(i64, f64, f64)> {
        let db = self.db.lock();
        db.prepare("SELECT id, mtime, size FROM files WHERE status = 'indexed' AND chunks > 0 ORDER BY id")
            .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as f64)))?.collect())
            .unwrap_or_default()
    }

    pub fn map_file_count(&self) -> i64 {
        let db = self.db.lock();
        db.query_row("SELECT COUNT(*) FROM files WHERE status = 'indexed' AND chunks > 0", [], |r| r.get(0))
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
        db.query_row("SELECT id, size, mtime, status FROM files WHERE path = ?", [path], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()
        .ok()
        .flatten()
    }

    /// A folder's unreadable files, newest first; "seen": its warning was looked at.
    pub fn errors(&self, folder_id: i64, limit: i64) -> Vec<Value> {
        let db = self.db.lock();
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
        self.changed();
    }

    /// Replace the vectors and keyword entries of the given files and upsert their records.
    pub fn save_files(&self, entries: &[Entry], vector_rows: &[VectorRow]) -> Result<()> {
        let at = now();
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        // The folder may have been removed while this group was being embedded.
        let live: HashSet<i64> = {
            let mut st = tx.prepare("SELECT id FROM folders")?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
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
            let mut vec_insert = tx.prepare("INSERT OR REPLACE INTO vectors (id, folder_id, kind, vector) VALUES (?, ?, ?, ?)")?;
            let mut fts = tx.prepare("INSERT INTO chunks_fts (rowid, name, text) VALUES (?, ?, ?)")?;
            for r in &vector_rows {
                let Some(&file_id) = ids.get(&r.path) else { continue };
                let key = (file_id << FTS_SHIFT) | r.chunk;
                let kind = kind_code(&r.kind);
                vec_insert.execute(params![key, r.folder_id, kind as i64, to_blob(&r.vector)])?;
                let name = if r.chunk == 0 { names.get(&r.path).cloned().unwrap_or_default() } else { String::new() };
                fts.execute(params![key, name, r.text])?;
                b_ids.push(file_id);
                b_chunks.push(r.chunk as i32);
                b_kinds.push(kind);
                b_folders.push(r.folder_id);
                b_vecs.extend_from_slice(&r.vector);
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
            for p in paths {
                let prefix = dir_prefix(p);
                let mut st =
                    db.prepare("SELECT id FROM files WHERE path = ? OR substr(path, 1, ?) = ? COLLATE NOCASE")?;
                let found = st.query_map(params![p, prefix.chars().count() as i64, prefix], |r| r.get::<_, i64>(0))?;
                for id in found {
                    ids.push(id?);
                }
                db.execute(
                    "DELETE FROM containers WHERE path = ? OR substr(path, 1, ?) = ? COLLATE NOCASE",
                    params![p, prefix.chars().count() as i64, prefix],
                )?;
            }
        }
        if !ids.is_empty() {
            self.remove_file_ids(&ids)?;
        }
        Ok(())
    }

    pub fn remove_file_ids(&self, ids: &[i64]) -> Result<()> {
        let mut db = self.db.lock();
        let tx = db.transaction()?;
        for part in ids.chunks(500) {
            Self::delete_fts(&tx, part)?;
            for &id in part {
                let (lo, hi) = fts_range(id);
                tx.execute("DELETE FROM vectors WHERE id BETWEEN ? AND ?", [lo, hi])?;
            }
            tx.execute(&format!("DELETE FROM files WHERE id IN ({})", ids_sql(part)), [])?;
        }
        let gone: HashSet<i64> = ids.iter().copied().collect();
        self.vectors.write().drop_rows(&|f, _| gone.contains(&f));
        tx.commit()?;
        drop(db);
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
        let db = self.db.lock();
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
        let db = self.db.lock();
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
            let prefix = dir_prefix(u);
            wheres.push("substr(f.path, 1, ?) = ? COLLATE NOCASE".into());
            values.push((prefix.chars().count() as i64).into());
            values.push(prefix.into());
        }
        // Rank first, then read the texts of just the best.
        let join = if wheres.len() > 1 { format!("JOIN files f ON f.id = (chunks_fts.rowid >> {FTS_SHIFT})") } else { String::new() };
        let sql = format!(
            "SELECT chunks_fts.rowid AS rid, bm25(chunks_fts, 4.0, 1.0) AS rank FROM chunks_fts {join}
             WHERE {} ORDER BY rank LIMIT ?",
            wheres.join(" AND ")
        );
        values.push((limit as i64).into());
        let db = self.db.lock();
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
        let db = self.db.lock();
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
        let prefix = dir_prefix(path);
        let db = self.db.lock();
        db.prepare("SELECT id FROM files WHERE status = 'indexed' AND substr(path, 1, ?) = ? COLLATE NOCASE")
            .and_then(|mut st| st.query_map(params![prefix.chars().count() as i64, prefix], |r| r.get(0))?.collect())
            .unwrap_or_default()
    }

    /// The folders directly inside `path` that hold searchable files, with how many.
    pub fn subdirs(&self, path: &str) -> Vec<Value> {
        let prefix = dir_prefix(path);
        let n = prefix.chars().count();
        let db = self.db.lock();
        let rows: Vec<String> = db
            .prepare(
                "SELECT path FROM files WHERE status IN ('indexed', 'pending') AND substr(path, 1, ?) = ? COLLATE NOCASE",
            )
            .and_then(|mut st| st.query_map(params![n as i64, prefix], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for p in rows {
            let rest: String = p.chars().skip(n).collect();
            if let Some((name, _)) = rest.split_once('\\') {
                *counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        let mut list: Vec<(String, i64)> = counts.into_iter().collect();
        list.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        list.into_iter().map(|(name, count)| json!({"path": format!("{prefix}{name}"), "name": name, "count": count})).collect()
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

    /// (file_ids, vectors) of every chunk in the index.

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
        // Settings -> File types (see filetypes.rs): the kinds switched off, and the rules
        "kinds_off": [], "hidden_files": false, "program_folders": false,
        // Settings -> Diagnostics: a tab with what the app uses
        "diagnostics": false,
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
            path: p.into(), folder_id: folder, chunk: 0, kind: "text".into(), text: text.into(), vector: unit(i),
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
                path: path.into(), folder_id, chunk: i as i64, kind: kind.into(), text: t.to_string(), vector: vec![1.0; EMBED_DIM],
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
            .map(|(i, v)| VectorRow { path: path.into(), folder_id, chunk: i as i64, kind: kind.into(), text: format!("{path} #{i}"), vector: v })
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
            assert!(mem.iter().zip(disk).all(|(m, d)| (m.similarity - d.similarity).abs() < 1e-5));
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
        drop(s);
        let again = Store::open_at(&dir.join("state.db")).unwrap();
        assert!(again.vector_count() == 0 && again.signature(r"C:\a\f.x").is_none());
        assert!(again.folder(fid).is_some()); // the folders stay
        assert!(!again.map_path().exists()); // the map of the files that were
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
}
