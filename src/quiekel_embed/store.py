"""Persistent state: SQLite for folders, files, settings and keyword search; LanceDB for vectors.

Searches don't scan the vector table on disk: every passage's vector is also kept in memory
(`_Vectors`, about 1 KB per passage), which turns a search into a few milliseconds of matrix
maths. The "lean" memory setting turns that off; searches then go to the table on disk.
"""

import gc
import json
import logging
import sqlite3
import threading
import time
from dataclasses import dataclass
from datetime import timedelta

import numpy as np

from . import config

log = logging.getLogger(__name__)

# Bump when what goes into the index changes (e.g. how titles are built): forces a re-index.
INDEX_FORMAT = 2

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE COLLATE NOCASE,
    watch INTEGER NOT NULL DEFAULT 1,
    added_at REAL NOT NULL,
    last_scan_at REAL
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
    indexed_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS files_folder ON files(folder_id);
-- Keyword search over file names (+ their folders) and passage text.
-- rowid = file_id << 10 | chunk
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    name, text, tokenize = 'unicode61 remove_diacritics 2'
);
"""

FTS_SHIFT = 10  # up to 1024 chunks per file in the keyword index
PREFILTER_MAX = 30_000  # on disk: bigger folders are filtered after the vector search instead
COMPACT_EVERY = 200  # saves between compactions of the vector table (each save adds a piece)…
COMPACT_MIN_S = 300  # …and at most this often: old pieces are only deleted a minute after

def _vector_schema():
    import pyarrow as pa

    return pa.schema([
        pa.field("file_id", pa.int64()),
        pa.field("folder_id", pa.int64()),
        pa.field("chunk", pa.int32()),
        pa.field("kind", pa.string()),
        pa.field("text", pa.string()),
        pa.field("vector", pa.list_(pa.float32(), config.EMBED_DIM)),
    ])
KIND_CODES = {"doc": 0, "text": 1, "code": 2, "image": 3}
CODE_KINDS = {v: k for k, v in KIND_CODES.items()}


@dataclass
class FileRecord:
    id: int
    folder_id: int
    path: str
    size: int
    mtime: float
    kind: str
    status: str
    chunks: int
    error: str | None


def _fts_range(file_id: int) -> tuple[int, int]:
    return file_id << FTS_SHIFT, (file_id << FTS_SHIFT) | ((1 << FTS_SHIFT) - 1)


def _dir_prefix(path: str) -> str:
    """'C:\\x' -> 'C:\\x\\', for matching the paths of everything inside a folder."""
    return path.rstrip("\\/") + "\\"


def _arrow_rows(rows) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """file ids, chunks, kind codes, folder ids and vectors of rows read from the table."""
    if rows is None or not rows.num_rows:
        return (np.zeros(0, np.int64), np.zeros(0, np.int32), np.zeros(0, np.int8), np.zeros(0, np.int64),
                np.zeros((0, config.EMBED_DIM), np.float32))
    kinds = np.array([KIND_CODES.get(k, 9) for k in rows.column("kind").to_pylist()], np.int8)
    # NumPy's own copy: memory Arrow allocated isn't given back to Windows once its thread
    # has ended, while big NumPy arrays are (the "lean" setting relies on that).
    vecs = rows.column("vector").combine_chunks().flatten().to_numpy(zero_copy_only=False)
    return (rows.column("file_id").to_numpy().astype(np.int64), rows.column("chunk").to_numpy().astype(np.int32),
            kinds, rows.column("folder_id").to_numpy().astype(np.int64),
            np.array(vecs, dtype=np.float32).reshape(-1, config.EMBED_DIM))


class _Block:
    """Rows of vectors added together (a write, or everything at start-up)."""

    __slots__ = ("ids", "chunks", "kinds", "folders", "vecs", "alive")

    def __init__(self, ids, chunks, kinds, folders, vecs):
        self.ids, self.chunks, self.kinds, self.folders, self.vecs = ids, chunks, kinds, folders, vecs
        self.alive = np.ones(len(ids), bool)


class _Vectors:
    """Every passage's vector in memory, so a search is a matrix product of a few milliseconds
    instead of a scan of the table on disk (~60 ms for 80,000 passages). The store keeps it in
    step with its own writes. Writes add small blocks, merged into one now and then."""

    MAX_ROWS = 500_000  # ~500 MB: bigger indexes are searched on disk instead
    MAX_BLOCKS = 48

    def __init__(self):
        self.lock = threading.Lock()
        self.ready = False
        self.rows = 0
        self._blocks: list[_Block] = []

    def fill(self, ids, chunks, kinds, folders, vecs):
        self._blocks = [_Block(ids, chunks, kinds, folders, vecs)] if len(ids) else []
        self.rows = len(ids)
        self.ready = self.rows <= self.MAX_ROWS
        if not self.ready:
            self._blocks = []

    def clear(self):
        self._blocks, self.rows, self.ready = [], 0, False

    def add(self, ids, chunks, kinds, folders, vecs):
        if not self.ready or not len(ids):
            return
        self._blocks.append(_Block(ids, chunks, kinds, folders, vecs))
        self.rows += len(ids)
        if self.rows > self.MAX_ROWS:
            self.clear()
        elif len(self._blocks) > self.MAX_BLOCKS:
            self._merge()

    def drop(self, which):
        """Forget rows: which(block) -> bool array of the rows to forget."""
        dead = 0
        for b in self._blocks:
            gone = which(b) & b.alive
            if gone.any():
                b.alive &= ~gone
                self.rows -= int(gone.sum())
            dead += int((~b.alive).sum())
        if dead > max(1000, self.rows // 4):
            self._merge()

    def _merge(self):
        parts = [b for b in self._blocks if b.alive.any()]
        if not parts:
            self._blocks = []
            return
        pick = lambda name: np.concatenate([getattr(b, name)[b.alive] for b in parts])  # noqa: E731
        self._blocks = [_Block(pick("ids"), pick("chunks"), pick("kinds"), pick("folders"), pick("vecs"))]

    def _mask(self, b: _Block, kinds, folder_id, file_ids):
        m = b.alive
        if kinds is not None:
            m = m & np.isin(b.kinds, kinds)
        if folder_id:
            m = m & (b.folders == folder_id)
        if file_ids is not None:
            m = m & np.isin(b.ids, file_ids)
        return m

    def search(self, q: np.ndarray, limit: int, kinds=None, folder_id=None, file_ids=None) -> list[tuple]:
        """(file_id, chunk, kind code, cosine) of the closest passages, best first."""
        found = []
        for b in self._blocks:
            m = self._mask(b, kinds, folder_id, file_ids)
            if not m.any():
                continue
            sims = np.where(m, b.vecs @ q, -np.inf)
            k = min(limit, int(m.sum()))
            top = np.argpartition(-sims, k - 1)[:k]
            found.extend((float(sims[i]), b, int(i)) for i in top)
        found.sort(key=lambda f: -f[0])
        return [(int(b.ids[i]), int(b.chunks[i]), int(b.kinds[i]), s) for s, b, i in found[:limit]]

    def rows_of(self, file_ids) -> tuple[np.ndarray, np.ndarray]:
        """(file ids, vectors) of these files' passages (or of all, for None)."""
        ids, vecs = [], []
        for b in self._blocks:
            m = b.alive if file_ids is None else b.alive & np.isin(b.ids, file_ids)
            if m.any():
                ids.append(b.ids[m])
                vecs.append(b.vecs[m])
        if not ids:
            return np.zeros(0, np.int64), np.zeros((0, config.EMBED_DIM), np.float32)
        return np.concatenate(ids), np.concatenate(vecs)


class Store:
    def __init__(self):
        config.DATA_DIR.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        # Writes to the vector table. Compacting it holds only this one, so searches (which need
        # the main lock for SQLite) don't wait for it.
        self._table_lock = threading.Lock()
        self._db = sqlite3.connect(config.DB_PATH, check_same_thread=False)
        self._db.row_factory = sqlite3.Row
        self._db.execute("PRAGMA journal_mode=WAL")
        # In WAL mode this is still crash-safe; a power cut may only undo the last saves,
        # which are then indexed again.
        self._db.execute("PRAGMA synchronous=NORMAL")
        self._db.execute("PRAGMA foreign_keys=ON")
        self._db.executescript(SCHEMA)
        # (added in 0.8) unreadable files whose warning you hid
        if "muted" not in {r[1] for r in self._db.execute("PRAGMA table_info(files)")}:
            self._db.execute("ALTER TABLE files ADD COLUMN muted INTEGER NOT NULL DEFAULT 0")

        self._table = None  # the vector table: opened on first use (see `table`)
        self._open_lock = threading.Lock()
        self._reset_vectors = False
        self._check_index_format()
        # Folder stats are polled every second by the UI: only recount after a write.
        self._version = 0
        self._folders_cache: tuple[int, list[dict]] | None = None
        self._saves = 0  # since the vector table was last compacted
        self._compacted_at = time.monotonic()
        self.vectors = _Vectors()
        self._keep_vectors = True
        self._too_many = False  # more passages than fit in memory: search them on disk

    def _changed(self):
        self._version += 1

    @property
    def version(self) -> int:
        """Goes up with every change to the index."""
        return self._version

    @property
    def table(self):
        """The vector table, opened on first use: importing LanceDB takes about a second, and the
        page needn't wait for that (searches use the vectors in memory, read in the background)."""
        if self._table is None:
            with self._open_lock:
                if self._table is None:
                    import lancedb

                    db = lancedb.connect(str(config.VECTORS_DIR))
                    if self._reset_vectors:
                        db.drop_table("chunks", ignore_missing=True)
                    self._table = db.create_table("chunks", schema=_vector_schema(), exist_ok=True)
        return self._table

    def _check_index_format(self):
        """Vectors from different models, sizes or formats can't be mixed: start over if any changed."""
        current = f"{config.MODEL_ID}@{config.EMBED_DIM}/f{INDEX_FORMAT}"
        if self.meta_get("index") not in (None, current):
            self._reset_vectors = True  # (the table is dropped when it's opened)
            self._db.execute("DELETE FROM files")
            self._db.execute("DELETE FROM chunks_fts")
            (config.DATA_DIR / "map.npz").unlink(missing_ok=True)  # its files are gone
        self.meta_set("index", current)

    # ---- vectors in memory -------------------------------------------------

    def keep_vectors_in_memory(self, keep: bool):
        """The "fast" memory setting keeps them (and fills them now, in the background);
        "lean" lets them go and searches the table on disk."""
        self._keep_vectors = keep
        if keep:
            threading.Thread(target=self.load_vectors, name="vectors", daemon=True).start()
        else:
            with self.vectors.lock:
                self.vectors.clear()
            gc.collect()
            threading.Thread(target=lambda: self.table, name="vectors", daemon=True).start()  # open it now

    def load_vectors(self):
        """Read every vector into memory (about 0.1 s per 80,000 passages)."""
        if not self._keep_vectors or self.vectors.ready or self._too_many:
            return
        started = time.perf_counter()
        table = self.table  # opening it (the first time: importing LanceDB) needs none of our locks
        with self._lock, self.vectors.lock:  # (no write can slip in between)
            if self.vectors.ready:
                return
            n = table.count_rows()
            if n > _Vectors.MAX_ROWS:
                self._too_many = True
                log.info("%d passages: searching them on disk instead of in memory", n)
                return
            rows = table.search().select(["file_id", "chunk", "kind", "folder_id", "vector"]).limit(n).to_arrow() if n else None
            self.vectors.fill(*_arrow_rows(rows))
            del rows
            import pyarrow as pa

            pa.default_memory_pool().release_unused()  # what reading took, back to Windows
        log.info("%d passages in memory in %.2fs", n, time.perf_counter() - started)

    def _vectors_ready(self) -> bool:
        if self._keep_vectors and not self.vectors.ready and not self._too_many:
            self.load_vectors()
        return self.vectors.ready

    # ---- meta & settings -----------------------------------------------

    def meta_get(self, key: str) -> str | None:
        with self._lock:
            row = self._db.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
        return row["value"] if row else None

    def meta_set(self, key: str, value: str):
        with self._lock:
            self._db.execute(
                "INSERT INTO meta (key, value) VALUES (?, ?) "
                "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (key, value),
            )
            self._db.commit()

    # ---- folders -------------------------------------------------------

    def folders(self) -> list[dict]:
        with self._lock:
            if self._folders_cache and self._folders_cache[0] == self._version:
                return [dict(f) for f in self._folders_cache[1]]
            rows = self._db.execute(
                """
                SELECT f.id, f.path, f.watch, f.added_at, f.last_scan_at,
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
                GROUP BY f.id ORDER BY f.path
                """
            ).fetchall()
            result = [dict(r) for r in rows]
            self._folders_cache = (self._version, result)
        return [dict(f) for f in result]

    def folder(self, folder_id: int) -> dict | None:
        with self._lock:
            row = self._db.execute("SELECT * FROM folders WHERE id = ?", (folder_id,)).fetchone()
        return dict(row) if row else None

    def add_folder(self, path: str) -> int:
        with self._lock:
            cur = self._db.execute(
                "INSERT INTO folders (path, added_at) VALUES (?, ?)", (path, time.time())
            )
            self._db.commit()
            self._changed()
            return cur.lastrowid

    def remove_folder(self, folder_id: int):
        with self._lock:
            ids = [r[0] for r in self._db.execute(
                "SELECT id FROM files WHERE folder_id = ?", (folder_id,))]
            self._delete_fts(ids)
            with self._table_lock:
                self.table.delete(f"folder_id = {int(folder_id)}")
            with self.vectors.lock:
                self.vectors.drop(lambda b: b.folders == folder_id)
            self._too_many = False
            self._db.execute("DELETE FROM files WHERE folder_id = ?", (folder_id,))
            self._db.execute("DELETE FROM folders WHERE id = ?", (folder_id,))
            self._db.commit()
            self._changed()

    def set_watch(self, folder_id: int, watch: bool):
        with self._lock:
            self._db.execute("UPDATE folders SET watch = ? WHERE id = ?", (int(watch), folder_id))
            self._db.commit()
            self._changed()

    def mark_scanned(self, folder_id: int):
        with self._lock:
            self._db.execute(
                "UPDATE folders SET last_scan_at = ? WHERE id = ?", (time.time(), folder_id)
            )
            self._db.commit()
            self._changed()

    # ---- files ---------------------------------------------------------

    def file(self, file_id: int) -> FileRecord | None:
        with self._lock:
            row = self._db.execute(
                "SELECT id, folder_id, path, size, mtime, kind, status, chunks, error "
                "FROM files WHERE id = ?",
                (file_id,),
            ).fetchone()
        return FileRecord(**dict(row)) if row else None

    def first_passage(self, file_id: int) -> str:
        """The start of a file's text, as shown in search results ("" for pictures)."""
        with self._lock:
            row = self._db.execute("SELECT text FROM chunks_fts WHERE rowid = ?", (file_id << FTS_SHIFT,)).fetchone()
        return row["text"] if row else ""

    def files_by_ids(self, ids: list[int]) -> dict[int, FileRecord]:
        if not ids:
            return {}
        marks = ",".join("?" * len(ids))
        with self._lock:
            rows = self._db.execute(
                "SELECT id, folder_id, path, size, mtime, kind, status, chunks, error "
                f"FROM files WHERE id IN ({marks})",
                ids,
            ).fetchall()
        return {r["id"]: FileRecord(**dict(r)) for r in rows}

    def map_files(self) -> tuple[np.ndarray, np.ndarray]:
        """Every file with vectors, by id: (ids, [mtime, size] each)."""
        with self._lock:
            rows = self._db.execute(
                "SELECT id, mtime, size FROM files WHERE status = 'indexed' AND chunks > 0 ORDER BY id"
            ).fetchall()
        if not rows:
            return np.zeros(0, np.int64), np.zeros((0, 2), np.float64)
        return (np.array([r[0] for r in rows], np.int64),
                np.array([(r[1], r[2]) for r in rows], np.float64))

    def map_file_count(self) -> int:
        with self._lock:
            return self._db.execute(
                "SELECT COUNT(*) FROM files WHERE status = 'indexed' AND chunks > 0").fetchone()[0]

    def file_signatures(self, folder_id: int) -> dict[str, tuple[int, int, float, str]]:
        """path -> (id, size, mtime, status) for every known file in a folder."""
        with self._lock:
            rows = self._db.execute(
                "SELECT id, path, size, mtime, status FROM files WHERE folder_id = ?", (folder_id,)
            ).fetchall()
        return {r["path"]: (r["id"], r["size"], r["mtime"], r["status"]) for r in rows}

    def add_pending(self, folder_id: int, files: list[tuple[str, int, float, str, str]]):
        """Files just found and not indexed yet: searchable by name right away, by content once
        indexed. files: (path, size, mtime, kind, name words). Files already known are left alone."""
        now = time.time()
        with self._lock:
            fts = []
            for path, size, mtime, kind, name in files:
                row = self._db.execute(
                    """
                    INSERT INTO files (folder_id, path, size, mtime, kind, status, chunks, error, indexed_at)
                    VALUES (?, ?, ?, ?, ?, 'pending', 0, NULL, ?)
                    ON CONFLICT(path) DO NOTHING RETURNING id
                    """,
                    (folder_id, path, size, mtime, kind, now),
                ).fetchone()
                if row:
                    fts.append((row[0] << FTS_SHIFT, name, ""))
            self._db.executemany("INSERT INTO chunks_fts (rowid, name, text) VALUES (?, ?, ?)", fts)
            self._db.commit()
            if fts:
                self._changed()

    def signature(self, path: str) -> tuple[int, int, float] | None:
        with self._lock:
            row = self._db.execute(
                "SELECT id, size, mtime FROM files WHERE path = ?", (path,)
            ).fetchone()
        return (row["id"], row["size"], row["mtime"]) if row else None

    def errors(self, folder_id: int, limit: int = 500) -> list[dict]:
        """A folder's unreadable files, newest first; "seen": its warning was looked at."""
        with self._lock:
            rows = self._db.execute(
                "SELECT id, path, error, muted FROM files WHERE folder_id = ? AND status = 'error' "
                "ORDER BY muted, indexed_at DESC LIMIT ?",
                (folder_id, limit),
            ).fetchall()
        return [{"id": r["id"], "path": r["path"], "error": r["error"], "seen": bool(r["muted"])} for r in rows]

    def errors_seen(self, folder_id: int):
        """A folder's unreadable files were looked at: no more warning for them. Files that fail
        later warn again, and so does one that was read fine in between and broke again."""
        with self._lock:
            self._db.execute("UPDATE files SET muted = 1 WHERE folder_id = ? AND status = 'error' AND muted = 0",
                             (folder_id,))
            self._db.commit()
            self._changed()

    def save_files(self, entries: list[dict], vectors_rows: list[dict]):
        """Replace the vectors and keyword entries of the given files and upsert their records.

        entries: dicts with folder_id, path, size, mtime, kind, status, chunks, error
                 and optionally search_name (file name + folders, for keyword search).
        vectors_rows: rows for the vector table's schema, with "path" in place of file_id.
        """
        now = time.time()
        with self._lock:
            # The folder may have been removed while this batch was being embedded.
            live = {r[0] for r in self._db.execute("SELECT id FROM folders")}
            entries = [e for e in entries if e["folder_id"] in live]
            vectors_rows = [r for r in vectors_rows if r["folder_id"] in live]
            # Only files that had vectors need them deleted: new files (most of a first indexing
            # run) don't, and skipping that saves a scan of the table and a new table version.
            had = set()
            paths = [e["path"] for e in entries]
            for i in range(0, len(paths), 500):
                part = paths[i:i + 500]
                had.update(r[0] for r in self._db.execute(
                    f"SELECT id FROM files WHERE chunks > 0 AND path IN ({','.join('?' * len(part))})", part))
            ids, names = {}, {}
            for e in entries:
                cur = self._db.execute(
                    """
                    INSERT INTO files (folder_id, path, size, mtime, kind, status, chunks, error, indexed_at)
                    VALUES (:folder_id, :path, :size, :mtime, :kind, :status, :chunks, :error, :now)
                    ON CONFLICT(path) DO UPDATE SET
                        folder_id = excluded.folder_id, size = excluded.size,
                        mtime = excluded.mtime, kind = excluded.kind, status = excluded.status,
                        chunks = excluded.chunks, error = excluded.error,
                        indexed_at = excluded.indexed_at,
                        muted = CASE WHEN excluded.status = 'error' THEN muted ELSE 0 END
                    RETURNING id
                    """,
                    {**e, "now": now},
                )
                ids[e["path"]] = cur.fetchone()[0]
                names[e["path"]] = e.get("search_name") or ""

            self._delete_fts(list(ids.values()))
            stale = [i for i in ids.values() if i in had]
            if stale:
                with self._table_lock:
                    self.table.delete(f"file_id IN ({','.join(map(str, stale))})")
            rows, fts = [], []
            for r in vectors_rows:
                r = dict(r)
                path = r.pop("path")
                r["file_id"] = ids[path]
                rows.append(r)
                fts.append(((r["file_id"] << FTS_SHIFT) | r["chunk"], names[path] if r["chunk"] == 0 else "", r["text"]))
            if rows:
                with self._table_lock:
                    self.table.add(rows)
                self._db.executemany("INSERT INTO chunks_fts (rowid, name, text) VALUES (?, ?, ?)", fts)
            with self.vectors.lock:
                if stale:
                    self.vectors.drop(lambda b: np.isin(b.ids, stale))
                if rows:
                    self.vectors.add(
                        np.array([r["file_id"] for r in rows], np.int64), np.array([r["chunk"] for r in rows], np.int32),
                        np.array([KIND_CODES.get(r["kind"], 9) for r in rows], np.int8),
                        np.array([r["folder_id"] for r in rows], np.int64),
                        np.array([r["vector"] for r in rows], np.float32).reshape(-1, config.EMBED_DIM))
            # Commit SQLite last, so a crash never marks a file indexed without its vectors.
            self._db.commit()
            self._changed()
            if rows or stale:
                self._saves += 1
        if self._saves >= COMPACT_EVERY and time.monotonic() - self._compacted_at >= COMPACT_MIN_S:
            self.optimize()

    def remove_paths(self, paths: list[str]):
        """Forget files, and everything below any path that was a directory."""
        with self._lock:
            ids = []
            for p in paths:
                prefix = p.rstrip("\\/") + "\\"
                rows = self._db.execute(
                    "SELECT id FROM files WHERE path = ? "
                    "OR substr(path, 1, ?) = ? COLLATE NOCASE",
                    (p, len(prefix), prefix),
                ).fetchall()
                ids.extend(r["id"] for r in rows)
        if ids:
            self.remove_file_ids(ids)

    def remove_file_ids(self, ids: list[int]):
        with self._lock:
            for i in range(0, len(ids), 500):
                part = ids[i : i + 500]
                self._delete_fts(part)
                with self._table_lock:
                    self.table.delete(f"file_id IN ({','.join(map(str, part))})")
                self._db.execute(
                    f"DELETE FROM files WHERE id IN ({','.join('?' * len(part))})", part
                )
            with self.vectors.lock:
                self.vectors.drop(lambda b: np.isin(b.ids, ids))
            self._db.commit()
            self._changed()
            self._saves += 1
            self._too_many = False  # (maybe they fit in memory now)

    def _delete_fts(self, ids: list[int]):
        self._db.executemany(
            "DELETE FROM chunks_fts WHERE rowid BETWEEN ? AND ?", [_fts_range(i) for i in ids]
        )

    # ---- search ----------------------------------------------------------

    def search(self, vector: np.ndarray, limit: int, kinds: tuple[str, ...] = (), folder_id: int | None = None,
               file_ids: list[int] | None = None) -> list[dict]:
        """The passages closest in meaning to `vector`, best first; optionally only of some kinds,
        one folder, or the given files."""
        q = (vector / (np.linalg.norm(vector) or 1.0)).astype(np.float32)
        if file_ids is not None and not file_ids:
            return []
        if self._vectors_ready():
            codes = np.array([KIND_CODES[k] for k in kinds], np.int8) if kinds else None
            with self.vectors.lock:
                hits = self.vectors.search(q, limit, codes, folder_id,
                                           None if file_ids is None else np.asarray(file_ids, np.int64))
            texts = self._passages([(fid << FTS_SHIFT) | chunk for fid, chunk, _, _ in hits])
            return [{"file_id": fid, "chunk": chunk, "kind": CODE_KINDS.get(kind, ""),
                     "text": texts.get((fid << FTS_SHIFT) | chunk, ""), "_distance": 1.0 - sim}
                    for fid, chunk, kind, sim in hits]
        return self._search_on_disk(q, limit, kinds, folder_id, file_ids)

    def _search_on_disk(self, q, limit, kinds, folder_id, file_ids) -> list[dict]:
        where = []
        if kinds:
            where.append("kind IN (" + ", ".join(f"'{k}'" for k in kinds) + ")")
        if folder_id:
            where.append(f"folder_id = {int(folder_id)}")
        inside = None
        if file_ids is not None:
            if len(file_ids) <= PREFILTER_MAX:
                where.append(f"file_id IN ({','.join(str(int(i)) for i in file_ids)})")
            else:  # too many to list: filter afterwards
                inside, limit = set(file_ids), limit * 8
        with self._lock:
            s = self.table.search(q).distance_type("cosine").limit(limit)
            if where:
                s = s.where(" AND ".join(where), prefilter=True)
            hits = s.select(["file_id", "chunk", "kind", "text", "_distance"]).to_list()
        return hits if inside is None else [h for h in hits if h["file_id"] in inside]

    def _passages(self, keys: list[int]) -> dict[int, str]:
        """Passage texts by keyword-index row id (file_id << 10 | chunk)."""
        out = {}
        with self._lock:
            for i in range(0, len(keys), 500):
                part = keys[i:i + 500]
                out.update(self._db.execute(
                    f"SELECT rowid, text FROM chunks_fts WHERE rowid IN ({','.join(map(str, part))})").fetchall())
        return out

    def keyword_search(
        self, match: str, limit: int, kinds: tuple[str, ...] = (), folder_id: int | None = None,
        under: str | None = None,
    ) -> list[dict]:
        """Full-text matches, best first. `match` is an FTS5 query string."""
        where, params = ["chunks_fts MATCH ?"], [match]
        if kinds:
            where.append(f"f.kind IN ({','.join('?' * len(kinds))})")
            params.extend(kinds)
        if folder_id:
            where.append("f.folder_id = ?")
            params.append(folder_id)
        if under:
            prefix = _dir_prefix(under)
            where.append("substr(f.path, 1, ?) = ? COLLATE NOCASE")
            params.extend([len(prefix), prefix])
        # Rank first, then read the texts of just the best: sorting thousands of matches with
        # their texts attached takes twice as long.
        join = f"JOIN files f ON f.id = (chunks_fts.rowid >> {FTS_SHIFT})" if len(where) > 1 else ""
        sql = f"""
            SELECT chunks_fts.rowid AS rid, bm25(chunks_fts, 4.0, 1.0) AS rank
            FROM chunks_fts {join}
            WHERE {" AND ".join(where)}
            ORDER BY rank LIMIT ?
        """
        with self._lock:
            try:
                ranked = self._db.execute(sql, [*params, limit]).fetchall()
            except sqlite3.OperationalError:
                return []  # e.g. a query FTS5 can't parse
            texts = {r["rowid"]: (r["name"], r["text"]) for r in self._db.execute(
                f"SELECT rowid, name, text FROM chunks_fts WHERE rowid IN ({','.join(str(r['rid']) for r in ranked) or '-1'})")}
        return [
            {"file_id": r["rid"] >> FTS_SHIFT, "chunk": r["rid"] & ((1 << FTS_SHIFT) - 1),
             "name": texts[r["rid"]][0], "text": texts[r["rid"]][1], "rank": r["rank"]}
            for r in ranked
        ]

    def vectors_of(self, file_ids: list[int]) -> tuple[np.ndarray, np.ndarray]:
        """(file ids, vectors) of these files' passages."""
        if not file_ids:
            return np.zeros(0, np.int64), np.zeros((0, config.EMBED_DIM), np.float32)
        if self._vectors_ready():
            with self.vectors.lock:
                return self.vectors.rows_of(np.asarray(file_ids, np.int64))
        ids, vecs = [], []
        for i in range(0, len(file_ids), 2000):
            part = file_ids[i:i + 2000]
            with self._lock:
                rows = (
                    self.table.search()
                    .where(f"file_id IN ({','.join(str(int(f)) for f in part)})")
                    .select(["file_id", "vector"])
                    .limit(len(part) * (config.MAX_CHUNKS_PER_FILE + config.SCANNED_PDF_PAGES))
                    .to_arrow()
                )
            if rows.num_rows:
                ids.append(rows.column("file_id").to_numpy())
                vecs.append(rows.column("vector").combine_chunks().flatten().to_numpy(zero_copy_only=False)
                            .reshape(-1, config.EMBED_DIM))
        if not ids:
            return np.zeros(0, np.int64), np.zeros((0, config.EMBED_DIM), np.float32)
        return np.concatenate(ids).astype(np.int64), np.concatenate(vecs).astype(np.float32)

    def file_vectors(self, file_id: int) -> np.ndarray:
        return self.vectors_of([int(file_id)])[1]

    def file_ids_under(self, path: str) -> list[int]:
        """Indexed files anywhere inside a folder."""
        prefix = _dir_prefix(path)
        with self._lock:
            rows = self._db.execute(
                "SELECT id FROM files WHERE status = 'indexed' AND substr(path, 1, ?) = ? COLLATE NOCASE",
                (len(prefix), prefix),
            ).fetchall()
        return [r["id"] for r in rows]

    def subdirs(self, path: str) -> list[dict]:
        """The folders directly inside `path` that hold searchable files, with how many."""
        prefix = _dir_prefix(path)
        with self._lock:
            rows = self._db.execute(
                "SELECT path FROM files WHERE status IN ('indexed', 'pending') "
                "AND substr(path, 1, ?) = ? COLLATE NOCASE",
                (len(prefix), prefix),
            ).fetchall()
        counts: dict[str, int] = {}
        for (p,) in rows:
            rest = p[len(prefix):]
            if "\\" in rest:
                name = rest.split("\\", 1)[0]
                counts[name] = counts.get(name, 0) + 1
        return [{"path": prefix + name, "name": name, "count": n}
                for name, n in sorted(counts.items(), key=lambda kv: kv[0].lower())]

    def similarities(self, vector: np.ndarray, file_ids: list[int]) -> dict[int, float]:
        """Each file's best cosine similarity to `vector`, over its chunks."""
        ids, vecs = self.vectors_of(file_ids)
        if not len(ids):
            return {}
        q = vector / (np.linalg.norm(vector) or 1.0)
        sims = vecs @ q / np.maximum(np.linalg.norm(vecs, axis=1), 1e-9)
        best: dict[int, float] = {}
        for fid, s in zip(ids.tolist(), sims.tolist()):
            if s > best.get(fid, -2.0):
                best[fid] = s
        return best

    def all_vectors(self) -> tuple[np.ndarray, np.ndarray]:
        """(file_ids, vectors) of every chunk in the index, without their texts."""
        if self._vectors_ready():
            with self.vectors.lock:
                return self.vectors.rows_of(None)
        with self._lock:
            n = self.table.count_rows()
            if not n:
                return np.zeros(0, np.int64), np.zeros((0, config.EMBED_DIM), np.float32)
            rows = self.table.search().select(["file_id", "vector"]).limit(n).to_arrow()
        ids = rows.column("file_id").to_numpy()
        vectors = rows.column("vector").combine_chunks().flatten().to_numpy(zero_copy_only=False)
        return ids, vectors.reshape(-1, config.EMBED_DIM).astype(np.float32)

    def vector_count(self) -> int:
        with self._lock:
            return self.table.count_rows()

    def optimize(self):
        """Compact the table's small pieces (each save adds one) and drop old versions, so disk
        use stays flat. Searches go on meanwhile: the table keeps serving its last version."""
        self._saves, self._compacted_at = 0, time.monotonic()
        with self._table_lock:
            try:
                self.table.optimize(cleanup_older_than=timedelta(minutes=1))
            except Exception:
                log.info("Compacting the vector table failed", exc_info=True)  # an optimisation only


class Settings:
    """User settings, kept in the meta table."""

    # Update checks are off until you turn them on: nothing goes online that you didn't ask for.
    DEFAULTS = {"perf_mode": "balanced", "free_gpu_idle": True, "check_updates": False, "language": "auto",
                "theme": "auto", "search_zips": True, "view_files": "cards", "view_images": "grid",
                "memory": "fast"}

    def __init__(self, store: Store):
        self._store = store
        self._values = dict(self.DEFAULTS)
        for key in self.DEFAULTS:
            raw = store.meta_get(f"setting.{key}")
            if raw is not None:
                self._values[key] = json.loads(raw)

    def get(self, key: str):
        return self._values[key]

    def all(self) -> dict:
        return dict(self._values)

    def update(self, **values):
        for key, value in values.items():
            if key not in self.DEFAULTS:
                raise KeyError(key)
            self._values[key] = value
            self._store.meta_set(f"setting.{key}", json.dumps(value))
