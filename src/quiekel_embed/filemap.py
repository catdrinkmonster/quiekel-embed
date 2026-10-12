"""A 3D map of the index: every file is a point, and files with similar meaning sit together.

Each file's vector (the mean of its passages) is squeezed from 256 dimensions into 3: PCA
first, then t-SNE, which keeps neighbours together. That takes about a minute for 6,000 files,
so the layout is kept (on disk too) and changed as little as possible:

- after a restart, the saved layout is there right away;
- new files are placed next to their most similar files, and every other point stays put;
- only once much has changed is everything laid out again, starting from the current layout
  and turned to match it, so the overall picture stays where it was.
"""

import json
import logging
import os
import threading
import time
from dataclasses import dataclass

import numpy as np

from . import config
from .governor import set_background_priority

log = logging.getLogger(__name__)

MAX_POINTS = 6000  # t-SNE gets slow beyond this: bigger indexes show a sample
REFRESH_S = 10  # while files are being indexed, the map catches up at most this often
TSNE_MIN = 150  # below this, t-SNE is unreliable and PCA alone keeps neighbours together better
REDO_SHARE = 0.3  # lay everything out again once this share of the points is new or only roughly placed
NEIGHBOURS = 8  # a new file goes where its most similar files are
SAVED_FORMAT = 1


def per_file(file_ids: np.ndarray, vectors: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """One unit vector per file: the mean of its chunk vectors."""
    if not len(file_ids):
        return file_ids, vectors
    order = np.argsort(file_ids, kind="stable")
    ids, vecs = file_ids[order], vectors[order].astype(np.float32)
    starts = np.flatnonzero(np.r_[True, ids[1:] != ids[:-1]])
    counts = np.diff(np.r_[starts, len(ids)])
    means = np.add.reduceat(vecs, starts, axis=0) / counts[:, None]
    means /= np.maximum(np.linalg.norm(means, axis=1, keepdims=True), 1e-9)
    return ids[starts], means


def to_3d(vectors: np.ndarray, seed: int = 0, init: np.ndarray | None = None) -> np.ndarray:
    """Coordinates of roughly [-1, 1]³, where nearby points have similar vectors. `init`: where
    the points are now, so that a new layout keeps the picture (it is also turned to match it)."""
    n = len(vectors)
    if n == 0:
        return np.zeros((0, 3), np.float32)
    x = vectors - vectors.mean(axis=0)
    u, s, _ = np.linalg.svd(x, full_matrices=False)
    k = max(1, min(50, n - 1, x.shape[1]))
    reduced = u[:, :k] * s[:k]
    if n >= TSNE_MIN:
        from sklearn.manifold import TSNE
        from threadpoolctl import threadpool_limits

        start = "pca"
        if init is not None:
            # t-SNE starts best from a tiny layout, as from its own PCA one.
            start = (init - init.mean(axis=0)) / (float(init[:, 0].std()) or 1.0) * 1e-4
        with threadpool_limits(2):  # stay gentle: this runs next to whatever else you do
            coords = TSNE(n_components=3, perplexity=min(30.0, (n - 1) / 3), init=start,
                          learning_rate="auto", random_state=seed, n_jobs=2).fit_transform(reduced)
    else:
        coords = np.zeros((n, 3))
        coords[:, : min(3, k)] = reduced[:, :3]
    coords = _fit(coords)
    return coords if init is None else align(coords, init)


def _fit(coords: np.ndarray) -> np.ndarray:
    coords = coords - coords.mean(axis=0)
    scale = float(np.quantile(np.abs(coords), 0.98)) or 1.0
    return np.clip(coords / scale, -1.15, 1.15).astype(np.float32)


def align(coords: np.ndarray, target: np.ndarray) -> np.ndarray:
    """Turn, mirror and scale `coords` to lie as close as possible to `target` (same points)."""
    if len(coords) < 4:
        return coords
    a, b = coords - coords.mean(axis=0), target - target.mean(axis=0)
    u, s, vt = np.linalg.svd(a.T @ b)
    scale = float(s.sum() / ((a ** 2).sum() or 1.0))
    return np.clip(a @ (u @ vt) * scale + target.mean(axis=0), -1.15, 1.15).astype(np.float32)


def sample(ids: np.ndarray, n: int) -> np.ndarray:
    """The same n files every time, as far as the index allows: the ones whose ids hash lowest.
    A new file only joins (and pushes one out) if its hash is lower, so the map changes little."""
    if len(ids) <= n:
        return ids
    h = ids.astype(np.uint64) * np.uint64(0x9E3779B97F4A7C15)
    return np.sort(ids[np.argpartition(h, n - 1)[:n]])


def place(new: np.ndarray, vectors: np.ndarray, coords: np.ndarray, new_ids: np.ndarray) -> np.ndarray:
    """Where new files go: among their most similar files on the map, with a little spread."""
    sims = new @ vectors.T
    k = min(NEIGHBOURS, len(vectors))
    top = np.argpartition(-sims, k - 1, axis=1)[:, :k]
    s = np.take_along_axis(sims, top, axis=1)
    w = np.exp((s - s.max(axis=1, keepdims=True)) / 0.02)  # mostly the very closest ones
    pos = (w[:, :, None] * coords[top]).sum(axis=1) / w.sum(axis=1, keepdims=True)
    spread = np.array([np.random.default_rng(int(i)).normal(0, 0.015, 3) for i in new_ids]).reshape(-1, 3)
    return np.clip(pos + spread, -1.15, 1.15).astype(np.float32)


@dataclass
class Layout:
    ids: np.ndarray  # sorted
    coords: np.ndarray  # (n, 3)
    vectors: np.ndarray  # (n, dim), unit length
    stamps: np.ndarray  # (n, 2): mtime and size when placed, to notice changed files
    rough: np.ndarray  # (n,) placed next to similar files rather than laid out


class FileMap:
    def __init__(self, store, path=None):
        self.store = store
        self.path = path or config.DATA_DIR / "map.npz"
        self._lock = threading.Lock()
        self._layout: Layout | None = None
        self._loaded = False
        self._body = b""  # the map as JSON, minus its status, ready to send
        self._points: list[dict] = []
        self._total = 0
        # Counts layouts shown; starts from the clock, so the page never takes a map from before
        # a restart for the current one.
        self._rev = int(time.time() * 1000) % 1_000_000_000
        self._version = -1
        self._computed_at = 0.0
        self._running = False

    # ---- what the page asks for ------------------------------------------------------

    def get(self) -> dict:
        status = self._status()
        if self._layout is None:
            return {"status": status}
        return {"status": status, "points": self._points, "total": self._total, "rev": self._rev}

    def reply(self) -> tuple[bytes, str]:
        """The map as JSON, and a tag that changes whenever the reply does."""
        status = self._status()
        with self._lock:
            if self._layout is None:
                return json.dumps({"status": status}).encode(), f'"none-{status}"'
            return b'{"status":"%s",' % status.encode() + self._body[1:], f'"{self._rev}-{status}"'

    def related(self, vector: np.ndarray) -> dict:
        """Cosine similarity of every dot on the map to `vector`."""
        with self._lock:
            layout = self._layout
        if layout is None:
            return {"ids": [], "sims": []}
        sims = layout.vectors @ vector.astype(np.float32)
        # (rounded as float64: float32 values would print as 0.12300000339746475)
        return {"ids": layout.ids.tolist(), "sims": np.round(sims.astype(np.float64), 3).tolist()}

    def dimension(self, index: int) -> dict:
        """Every dot's value in one of the dimensions."""
        with self._lock:
            layout = self._layout
        if layout is None:
            return {"ids": [], "values": []}
        return {"ids": layout.ids.tolist(), "values": np.round(layout.vectors[:, index].astype(np.float64), 4).tolist()}

    def _status(self) -> str:
        """'ready', 'updating' (the index changed since; catching up) or 'computing' (no map yet)."""
        self._load_saved()
        version = self.store.version
        with self._lock:
            fresh = self._layout is not None and self._version == version
            due = time.monotonic() - self._computed_at > REFRESH_S
            if not fresh and due and not self._running:
                self._running = True
                threading.Thread(target=self._compute, args=(version,), name="map", daemon=True).start()
            if self._layout is None:
                return "computing"
            return "ready" if fresh else "updating"

    # ---- keeping the layout ---------------------------------------------------------------

    def _load_saved(self):
        if self._loaded:
            return
        with self._lock:
            if self._loaded:
                return
            self._loaded = True
            try:
                with np.load(self.path) as f:
                    if int(f["format"]) != SAVED_FORMAT or f["vectors"].shape[1] != config.EMBED_DIM:
                        return
                    layout = Layout(f["ids"], f["coords"], f["vectors"].astype(np.float32), f["stamps"], f["rough"])
            except FileNotFoundError:
                return
            except Exception:
                log.warning("Could not read the saved map; it will be laid out again", exc_info=True)
                return
            self._set(layout, self.store.map_file_count(), changed=True)

    def _save(self, layout: Layout):
        tmp = self.path.with_name(self.path.name + ".tmp.npz")
        try:
            np.savez(tmp, format=SAVED_FORMAT, ids=layout.ids, coords=layout.coords,
                     vectors=layout.vectors.astype(np.float16), stamps=layout.stamps, rough=layout.rough)
            os.replace(tmp, self.path)
        except OSError:
            log.warning("Could not save the map", exc_info=True)

    def _set(self, layout: Layout, total: int, changed: bool):
        """Make `layout` the one shown (call with the lock held)."""
        files = self.store.files_by_ids(layout.ids.tolist())
        keep = np.array([int(i) in files for i in layout.ids], dtype=bool)
        if not keep.all():  # files gone meanwhile
            layout = Layout(*(a[keep] for a in (layout.ids, layout.coords, layout.vectors, layout.stamps, layout.rough)))
        if changed or not keep.all():
            self._rev += 1
            self._points = [
                {"id": int(i), "x": round(float(x), 4), "y": round(float(y), 4), "z": round(float(z), 4),
                 "k": files[int(i)].kind, "p": files[int(i)].path}
                for i, (x, y, z) in zip(layout.ids, layout.coords)
            ]
        self._total = total
        self._body = json.dumps({"points": self._points, "total": total, "rev": self._rev},
                                separators=(",", ":")).encode()
        self._layout = layout

    def _compute(self, version: int):
        set_background_priority(True)
        started = time.monotonic()
        try:
            layout, total, how = self._next_layout()
            changed = layout is not self._layout
            with self._lock:
                self._set(layout, total, changed)
                self._version = version
            if changed:
                self._save(layout)
                log.info("Map of %d files %s in %.1fs", len(layout.ids), how, time.monotonic() - started)
        except Exception:
            log.exception("Could not compute the map")
        finally:
            with self._lock:
                self._computed_at, self._running = time.monotonic(), False

    def _next_layout(self) -> tuple[Layout, int, str]:
        ids, stamps = self.store.map_files()  # every file with vectors, by id
        total = len(ids)
        shown = sample(ids, MAX_POINTS)
        stamps = stamps[np.searchsorted(ids, shown)]
        old = self._layout
        if old is not None and len(old.ids):
            at = np.searchsorted(old.ids, shown).clip(0, len(old.ids) - 1)
            same = (old.ids[at] == shown) & (old.stamps[at] == stamps).all(axis=1)
        else:
            at, same = np.zeros(len(shown), int), np.zeros(len(shown), bool)
        if old is not None and same.all() and len(shown) == len(old.ids):
            return old, total, "unchanged"  # nothing on the map changed
        new_ids = shown[~same]
        new_vecs = self._vectors_of(new_ids)
        has = np.isin(new_ids, new_vecs[0])  # (a file may lose its vectors in between)
        if not has.all():
            drop = set(new_ids[~has].tolist())
            keep = np.array([i not in drop for i in shown.tolist()])
            shown, stamps, at, same = shown[keep], stamps[keep], at[keep], same[keep]
            new_ids = shown[~same]
        vectors = np.zeros((len(shown), config.EMBED_DIM), np.float32)
        coords = np.zeros((len(shown), 3), np.float32)
        rough = np.zeros(len(shown), bool)
        if same.any():
            vectors[same] = old.vectors[at[same]]
            coords[same] = old.coords[at[same]]
            rough[same] = old.rough[at[same]]
        if len(new_ids):
            vectors[~same] = new_vecs[1][np.searchsorted(new_vecs[0], new_ids)]
            if same.sum() >= NEIGHBOURS:
                coords[~same] = place(vectors[~same], vectors[same], coords[same], new_ids)
                rough[~same] = True
        n = len(shown)
        if same.sum() < NEIGHBOURS or n < TSNE_MIN or rough.sum() > REDO_SHARE * n:
            # Lay it all out again, starting from where everything is now.
            init = coords if same.sum() >= NEIGHBOURS else None
            coords = to_3d(vectors, init=init)
            rough[:] = False
            how = "laid out" if init is None else "laid out again"
        else:
            how = f"updated ({len(new_ids)} placed)"
        return Layout(shown, coords, vectors, stamps, rough), total, how

    def _vectors_of(self, file_ids: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        if not len(file_ids):
            return np.zeros(0, np.int64), np.zeros((0, config.EMBED_DIM), np.float32)
        return per_file(*self.store.vectors_of(file_ids.tolist()))
