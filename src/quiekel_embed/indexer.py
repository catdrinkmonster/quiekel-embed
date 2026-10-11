"""Background indexing: one worker thread, a job queue, and a file watcher.

The worker runs at Windows "background" priority (low CPU, disk and memory
priority) and asks the resource governor how fast it may go after every unit of
work. When there is nothing to do, or video memory runs out, it hands the GPU back.
"""

import collections
import logging
import os
import queue
import stat
import threading
import time
from pathlib import Path

from watchdog.events import FileSystemEventHandler
from watchdog.observers import Observer

from . import archive, config
from .embedder import Embedder, GpuBusy
from .extract import SkipFile, extract, max_bytes, problem
from .filetypes import Report, Types
from .governor import Governor, keeps_gpu, lean, set_background_priority
from .store import Settings, Store

log = logging.getLogger(__name__)

_HIDDEN = getattr(stat, "FILE_ATTRIBUTE_HIDDEN", 0)
_SYSTEM = getattr(stat, "FILE_ATTRIBUTE_SYSTEM", 0)
IDLE_RELEASE_S = 300  # hand the GPU back after this long without indexing work
GPU_NEEDED_GB = 1.2  # free video memory needed to put the model on the GPU
# Files read and decoded ahead, so the GPU never waits for the CPU: a whole group of images
# (see _Group.full), so the next group is ready when the GPU finishes this one. Fewer when
# memory is tight.
PREFETCH_FILES = 32
PREFETCH_TIGHT = 8


def attributes(entry: os.DirEntry) -> tuple[bool, bool]:
    """(hidden, system), as Windows marks them."""
    try:
        attrs = entry.stat(follow_symlinks=False).st_file_attributes
    except (AttributeError, OSError):
        return False, False
    return bool(attrs & _HIDDEN), bool(attrs & _SYSTEM)


def skipped_part(types: Types, parts) -> str | None:
    """Why a path's folders or name rule it out (inside an archive, or from a file event)."""
    *folders, name = parts
    for parent, folder in zip(["", *folders], folders):
        why = types.skip_dir(folder, False, False, parent)
        if why:
            return why
    return types.skip_file(name, False, False)


def by_container(signatures: dict) -> dict[str, list]:
    """The known files inside containers, by container: path -> [(path, size, mtime)]."""
    out: dict[str, list] = {}
    for path, (_, size, mtime, _status) in signatures.items():
        parts = path.split("\\")
        for k in range(1, len(parts)):
            if archive.container_type(parts[k - 1]):
                out.setdefault(os.path.normcase("\\".join(parts[:k])), []).append((path, size, mtime))
                break
    return out


def is_inside(path: str, root: str) -> bool:
    path, root = os.path.normcase(path), os.path.normcase(root.rstrip("\\/"))
    return path == root or path.startswith(root + os.sep)


def file_name(path: str) -> str:
    return path.rsplit("\\", 1)[-1]


def search_name(path: str, root: str) -> str:
    """Filename plus the folders it sits in, for keyword search ("Taxes 2024 landlord.pdf")."""
    rel = Path(os.path.relpath(path, root))
    return " ".join([rel.name, *rel.parent.parts])


def embed_title(path: str, root: str) -> str:
    """Title given to the model: the file name, with its folder when that adds meaning."""
    rel = Path(os.path.relpath(path, root))
    return f"{rel.parent.name}/{rel.name}" if rel.parent.name else rel.name


class Indexer:
    def __init__(self, store: Store, embedder: Embedder, governor: Governor, settings: Settings):
        self.store = store
        self.embedder = embedder
        self.governor = governor
        self.settings = settings
        self.jobs: queue.Queue = queue.Queue()
        self._paused = threading.Event()
        self._retry = threading.Event()
        self._waking = threading.Event()
        self._cancelled: set[int] = set()
        self._progress_lock = threading.Lock()
        self._progress: dict = {"state": "starting"}
        self._last_work = time.monotonic()
        self._background = None
        self._embedding = False
        self._recent: collections.deque = collections.deque(maxlen=120)
        embedder.pace = self._pace
        self.watcher = Watcher(self)
        self._thread = threading.Thread(target=self._run, name="indexer", daemon=True)

    # ---- public API (called from web threads) --------------------------

    def start(self):
        self._thread.start()
        for f in self.store.folders():
            self.enqueue_scan(f["id"])  # catch changes made while the app was closed
        self.watcher.sync()

    def enqueue_scan(self, folder_id: int):
        self._cancelled.discard(folder_id)
        self.jobs.put(("scan", folder_id))

    def enqueue_paths(self, paths: list[str]):
        """Changes the watcher saw. They're noted right away, even while indexing is paused or busy
        (see _note_changes); reading the new content waits its turn."""
        if paths:
            for job in self._note_changes(paths):
                self.jobs.put(job)

    def remove_folder(self, folder_id: int):
        self._cancelled.add(folder_id)
        self.store.remove_folder(folder_id)
        self.watcher.sync()

    def pause(self):
        self._paused.set()

    def resume(self):
        self._paused.clear()

    def retry_model(self):
        self._retry.set()

    @property
    def paused(self) -> bool:
        return self._paused.is_set()

    def progress(self) -> dict:
        with self._progress_lock:
            p = dict(self._progress)
        p["queued"] = self.jobs.qsize()
        p["paused"] = self.paused
        p["files_per_min"] = self._files_per_min()
        return p

    def _set(self, **kw):
        with self._progress_lock:
            self._progress = kw

    def _update(self, **kw):
        with self._progress_lock:
            self._progress.update(kw)

    def _files_per_min(self) -> float | None:
        if len(self._recent) < 2:
            return None
        (t0, n0), (t1, n1) = self._recent[0], self._recent[-1]
        if t1 - t0 < 5 or time.monotonic() - t1 > 30:
            return None
        return round((n1 - n0) / (t1 - t0) * 60, 1)

    # ---- pacing & device -------------------------------------------------

    def _apply_priority(self):
        background = self.settings.get("perf_mode") != "full"
        self.embedder.background = background  # its helper thread follows
        if background != self._background:
            set_background_priority(background)
            self._background = background

    def _pace(self, work_s: float):
        """After each unit of work: let the governor slow us down (handing the GPU back
        if it pauses us because video memory ran out), honour a user pause, and if we're
        mid-embedding, make sure the model is back on the GPU before the next batch."""
        self.governor.pace(
            work_s,
            should_stop=self._paused.is_set,
            while_paused=lambda: self._manage_device(working=True),
        )
        self._wait_if_paused()
        if self._embedding:
            self._ensure_gpu_for_work()

    def _gpu_ok_now(self) -> bool:
        m = self.governor.metrics
        return m.vram_free_gb is None or m.vram_free_gb >= GPU_NEEDED_GB

    def _wait_if_paused(self):
        while self._paused.is_set():
            self._manage_device(working=False)
            time.sleep(0.5)

    def _manage_device(self, working: bool):
        """What stays loaded with nothing to do, by the Performance setting: Light lets the model go,
        Balanced gives the graphics card back, Maximum keeps it."""
        e = self.embedder
        if not e.ready:
            return
        unused = not working and time.monotonic() - max(self._last_work, e.last_used) > IDLE_RELEASE_S
        mode = self.settings.get("perf_mode")
        if unused and lean(mode):
            e.unload("not used for a while")  # wakes up again for the next search or new file
            return
        if not e.on_gpu:
            return
        # Handing the GPU back moves the model into RAM (ONNX Runtime lets go of it); Light lets go.
        let_go = e.unload if lean(mode) else e.release_gpu
        m = self.governor.metrics
        if m.vram_free_gb is not None and m.vram_free_gb < 0.3:
            let_go("GPU memory almost full")
        elif unused and not keeps_gpu(mode):
            let_go("nothing to index")

    def wake_model(self):
        """Load the model again after it went to sleep (Light performance), in the background."""
        e = self.embedder
        if e.ready or e.status != "asleep" or self._waking.is_set():
            return
        self._waking.set()

        def wake():
            try:
                e.load(prefer_gpu=self._gpu_ok_now())
            except Exception:
                log.exception("Could not wake the model")
            finally:
                self._waking.clear()

        threading.Thread(target=wake, name="wake-model", daemon=True).start()

    def _ensure_model(self):
        """New files to index while the model sleeps: wake it first."""
        if not self.embedder.ready:
            self.embedder.load(prefer_gpu=self._gpu_ok_now())

    def _ensure_gpu_for_work(self):
        """Indexing on the CPU would be slow and heavy, so wait for the GPU instead."""
        e = self.embedder
        if not e.cuda or e.on_gpu:
            return
        waiting = False
        while not e.on_gpu:
            m = self.governor.metrics
            if self.governor.decision.duty > 0 and not self._paused.is_set() and self._gpu_ok_now():
                if e.use_gpu():
                    break
            if not waiting and self.governor.decision.duty > 0:
                self._update(note={"key": "note.waiting_vram", "params": {"gb": round(m.vram_free_gb or 0, 1)}})
                waiting = True
            time.sleep(3)
        if waiting:
            self._update(note=None)

    # ---- worker --------------------------------------------------------

    def _run(self):
        self._apply_priority()
        while not self.embedder.ready:
            self._set(state="loading-model")
            try:
                # With little video memory, load onto the CPU instead.
                self.embedder.load(prefer_gpu=self._gpu_ok_now())
            except Exception:
                self._set(state="model-error", message=self.embedder.error)
                self._retry.wait()
                self._retry.clear()

        while True:
            self._set(state="idle")
            try:
                job, arg = self.jobs.get(timeout=5)
            except queue.Empty:
                self._apply_priority()
                self._manage_device(working=False)
                continue
            self._apply_priority()
            try:
                self._do(job, arg)
            except Exception as e:
                log.exception("Job %s failed", job)
                self._set(state="error", message=f"{type(e).__name__}: {e}")
                time.sleep(2)

    def _do(self, job: str, arg):
        if job == "scan":
            self._scan_folder(arg)
        elif job == "index":
            self._index_noted(*arg)
        elif job == "remove":
            # Again, in turn: the job before may have read one of them just before it went.
            self.store.remove_paths([p for p in arg if not os.path.exists(p)])

    def types(self) -> Types:
        """What's searched, as Settings -> File types say."""
        return Types.of(self.settings)

    def _walk(self, root: str, folder_id: int, report: Report | None = None, types: Types | None = None,
              containers: "_Containers | None" = None):
        """Yield (path, size, mtime) for the files below root that are searched, also inside
        archives and emails; note in `report` why everything else isn't."""
        report = report if report is not None else Report()
        types = types or self.types()
        containers = containers or _Containers(types)
        stack = [root]
        while stack:
            if folder_id in self._cancelled:
                return
            d = stack.pop()
            here = os.path.basename(d.rstrip("\\/"))
            try:
                with os.scandir(d) as it:
                    entries = list(it)
            except PermissionError:
                report.add("no_access_dir", d)
                continue
            except OSError:
                report.add("unreadable_dir", d)
                continue
            for e in entries:
                try:
                    if e.is_symlink() or getattr(e, "is_junction", lambda: False)():
                        report.add("link", e.path)  # it could lead anywhere, or in circles
                        continue
                    is_dir = e.is_dir(follow_symlinks=False)
                except OSError:
                    continue
                hidden, system = attributes(e)
                if is_dir:
                    why = types.skip_dir(e.name, hidden, system, here)
                    if why:
                        report.add(why, e.path)
                    elif not is_inside(e.path, str(config.DATA_DIR)):
                        stack.append(e.path)
                    continue
                why = types.skip_file(e.name, hidden, system)
                if why:
                    report.add(why, e.path)
                    continue
                kind, inside = types.kind(e.name, e.path), types.inside(e.name)
                if not kind and not inside:
                    report.add_type(types.why_not(e.name), e.path)
                    continue
                try:
                    st = e.stat(follow_symlinks=False)
                except OSError:
                    continue
                if kind:
                    yield e.path, st.st_size, st.st_mtime
                if inside:
                    yield from containers.files(e.path, st.st_size, st.st_mtime, report)

    def _scan_folder(self, folder_id: int):
        folder = self.store.folder(folder_id)
        if not folder or folder_id in self._cancelled:
            return
        root = folder["path"]
        self._set(state="scanning", folder=root, found=0)
        if not os.path.isdir(root):
            self._set(state="error", message=f"Folder not found: {root}")
            time.sleep(2)
            return

        types = self.types()
        known = self.store.file_signatures(folder_id)
        known_ci = {os.path.normcase(p): v for p, v in known.items()}
        containers = _Containers(types, self.store.containers(folder_id), by_container(known))
        report = Report()
        seen, todo = set(), []
        for n, (path, size, mtime) in enumerate(self._walk(root, folder_id, report, types, containers), 1):
            key = os.path.normcase(path)
            seen.add(key)
            sig = known_ci.get(key)
            # New or changed, not finished last time, or failed before (maybe readable now).
            if sig is None or sig[1] != size or abs(sig[2] - mtime) > 1e-3 or sig[3] in ("pending", "error"):
                todo.append((path, size, mtime))
            if n % 500 == 0:
                self._update(found=n)
        if folder_id in self._cancelled:
            return

        gone = [v[0] for k, v in known_ci.items() if k not in seen]
        if gone:
            self.store.remove_file_ids(gone)
        # Everything found is searchable by name right away; by content as indexing gets to it.
        self.store.add_pending(folder_id, [(p, size, mtime, types.kind(file_name(p), p) or "text", search_name(p, root))
                                           for p, size, mtime in todo])
        self._index_files(folder_id, root, todo, types)
        if folder_id not in self._cancelled:
            self.store.save_containers(folder_id, list(containers.seen.values()))
            self.store.mark_scanned(folder_id, report.data)
            if todo or gone:
                self.store.optimize()

    def _note_changes(self, paths: list[str]) -> list[tuple]:
        """Write down what the watcher saw, at once: new files are searchable by name and count as
        waiting, deleted ones disappear. This runs on the watcher's thread, so a paused or busy
        indexer doesn't hold it up. Returns the jobs that read the new content, in turn."""
        folders = [f for f in self.store.folders() if f["watch"]]
        types = self.types()
        removed, by_folder = [], {}
        for p in dict.fromkeys(paths):
            folder = next((f for f in folders if is_inside(p, f["path"])), None)
            if folder is None or is_inside(p, str(config.DATA_DIR)):
                continue
            rel = Path(os.path.relpath(p, folder["path"]))
            if rel.parts and skipped_part(types, rel.parts):
                continue
            files = by_folder.setdefault(folder["id"], (folder["path"], []))[1]
            if not os.path.exists(p):
                removed.append(p)
            elif os.path.isdir(p):
                # A folder appeared (e.g. moved in): index everything below it.
                files.extend(f for f in self._walk(p, folder["id"], types=types) if self._changed(f))
            else:
                name = file_name(p)
                kind, inside = types.kind(name, p), types.inside(name)
                if not kind and not inside:
                    continue
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                attrs = getattr(st, "st_file_attributes", 0)
                if types.skip_file(name, bool(attrs & _HIDDEN), bool(attrs & _SYSTEM)):
                    continue
                if kind and self._changed((p, st.st_size, st.st_mtime)):
                    files.append((p, st.st_size, st.st_mtime))
                if inside:
                    # An archive or an email changed: index what's new or changed inside it, and
                    # forget what's gone.
                    containers = _Containers(types)
                    inner = list(containers.files(p, st.st_size, st.st_mtime, Report()))
                    keep = {os.path.normcase(f[0]) for f in inner}
                    prefix = os.path.normcase(p) + "\\"
                    stale = [v[0] for q, v in self.store.file_signatures(folder["id"]).items()
                             if os.path.normcase(q).startswith(prefix) and os.path.normcase(q) not in keep]
                    if stale:
                        self.store.remove_file_ids(stale)
                    files.extend(f for f in inner if self._changed(f))
                    self.store.save_containers(folder["id"], list(containers.seen.values()), replace=False)
        jobs = []
        if removed:
            self.store.remove_paths(removed)
            jobs.append(("remove", removed))
        for folder_id, (root, files) in by_folder.items():
            if files:
                self.store.add_pending(folder_id, [(p, size, mtime, types.kind(file_name(p), p) or "text",
                                                    search_name(p, root)) for p, size, mtime in files])
                jobs.append(("index", (folder_id, root, files)))
        return jobs

    def _handle_paths(self, paths: list[str]):
        """Note changes and read them in one go (the watcher and the indexer do it in turn)."""
        for job, arg in self._note_changes(paths):
            self._do(job, arg)

    def _index_noted(self, folder_id: int, root: str, files: list):
        """Read the files the watcher noted, unless they went away or a scan got to them first."""
        if folder_id in self._cancelled:
            return
        todo = []
        for path, size, mtime in files:
            sig = self.store.signature(path)
            if sig and (sig[3] == "pending" or sig[1] != size or abs(sig[2] - mtime) > 1e-3):
                todo.append((path, size, mtime))
        self._index_files(folder_id, root, todo)

    def _changed(self, f) -> bool:
        sig = self.store.signature(f[0])
        return sig is None or sig[1] != f[1] or abs(sig[2] - f[2]) > 1e-3

    def _index_files(self, folder_id: int, root: str, files: list, types: Types | None = None):
        if not files:
            return
        types = types or self.types()
        total = len(files)
        self._set(state="indexing", folder=root, done=0, total=total, current="")
        self._recent.clear()
        # Reading and decoding files (CPU) runs ahead of embedding (GPU) in its own thread, so
        # the GPU isn't left waiting between groups: a steady load instead of bursts.
        ready: queue.Queue = queue.Queue(maxsize=PREFETCH_FILES)
        stop = threading.Event()
        threading.Thread(target=self._extract_ahead, args=(folder_id, root, files, types, ready, stop),
                         name="extract", daemon=True).start()
        group = _Group()
        try:
            for done in range(total):
                self._wait_if_paused()
                if folder_id in self._cancelled:
                    return
                item = self._take(ready)
                if item is None:  # the reader stopped early: the folder was removed
                    return
                entry, chunks = item
                self._recent.append((time.monotonic(), done))
                rate = self._files_per_min()
                self._update(
                    done=done, current=entry["path"],
                    eta_s=round((total - done) / rate * 60) if rate else None,
                )
                group.add(entry, chunks)
                self._last_work = time.monotonic()
                if group.full(self._group_scale()):
                    self._flush(group)
                    group = _Group()
            self._flush(group)
            self._update(done=total, current="", eta_s=None)
        finally:
            stop.set()
            while True:  # let the reader go if it's waiting to hand over a file
                try:
                    ready.get_nowait()
                except queue.Empty:
                    break

    def _take(self, ready: queue.Queue):
        """The next file from the reader. While waiting for it (the reader waits out pauses),
        hand the GPU back when video memory runs out, just as during embedding."""
        while True:
            try:
                return ready.get(timeout=0.5)
            except queue.Empty:
                pass
            self._wait_if_paused()
            if self.governor.decision.duty <= 0:
                self._manage_device(working=True)

    def _extract_ahead(self, folder_id: int, root: str, files: list, types: Types, ready: queue.Queue,
                       stop: threading.Event):
        """The reading side of the pipeline: files in, (entry, chunks) out, in order."""
        try:
            with archive.session():  # archives stay open while their files are read one by one
                self._read_all(folder_id, root, files, types, ready, stop)
        except Exception:
            log.exception("Reading ahead failed")
        # "No more files", even after an error, or the indexer would wait forever (unless it
        # has stopped already).
        while not stop.is_set():
            try:
                ready.put(None, timeout=0.25)
                break
            except queue.Full:
                pass

    def _read_all(self, folder_id: int, root: str, files: list, types: Types, ready: queue.Queue,
                  stop: threading.Event):
        set_background_priority(self.settings.get("perf_mode") != "full")

        def hand_over(item) -> bool:
            while not stop.is_set():
                if ready.qsize() >= PREFETCH_TIGHT and self._group_scale() < 1:
                    time.sleep(0.1)  # memory is tight: hold fewer files ahead
                    continue
                try:
                    ready.put(item, timeout=0.25)
                    return True
                except queue.Full:
                    continue
            return False

        for path, size, mtime in files:
            # Nothing to read ahead for while indexing waits (paused, memory full, …).
            while not stop.is_set() and (self._paused.is_set() or self.governor.decision.duty <= 0):
                time.sleep(0.25)
            if stop.is_set() or folder_id in self._cancelled:
                break
            started = time.perf_counter()
            item = self._read(folder_id, root, path, size, mtime, types)
            # Reading is paced like embedding, so a busy PC gets its CPU back too.
            self.governor.pace(time.perf_counter() - started, should_stop=lambda: stop.is_set() or self._paused.is_set())
            if not hand_over(item):
                break

    def _read(self, folder_id: int, root: str, path: str, size: int, mtime: float,
              types: Types) -> tuple[dict, list]:
        name = file_name(path)
        kind = types.kind(name, path) or "text"
        entry = {
            "folder_id": folder_id, "path": path, "size": size, "mtime": mtime,
            "kind": kind, "status": "indexed", "chunks": 0, "error": None,
            "search_name": search_name(path, root),
        }
        try:
            if not os.path.exists(path) and archive.split(path) is not None:  # inside an archive or an email
                limit = max_bytes(kind, name)
                if size > limit:
                    raise SkipFile("too_large")
                with archive.extracted(path, limit) as copy:
                    ex = extract(copy, size, title=embed_title(path, root), kind=kind)
            else:
                ex = extract(Path(path), size, title=embed_title(path, root), kind=kind)
            entry["kind"] = ex.kind
            return entry, ex.chunks
        except SkipFile as e:
            entry.update(status="skipped", error=f"!{e}")
        except archive.Encrypted:
            entry.update(status="skipped", error="!password")
        except archive.TooLarge:
            entry.update(status="skipped", error="!too_large")
        except Exception as e:
            entry.update(status="error", error=problem(e))
        return entry, []

    def _group_scale(self) -> float:
        # Fewer files (and decoded images) held in memory when RAM is tight.
        return 0.25 if self.governor.metrics.ram_free_gb < 2.5 else 1.0

    def _flush(self, group: "_Group"):
        if not group.entries:
            return
        rows = self._embed(group)
        if rows is None:
            # Isolate the file that broke the batch instead of failing them all.
            rows = []
            for entry, chunks in zip(group.entries, group.chunks):
                single = _Group()
                single.add(entry, chunks)
                one = self._embed(single)
                if one is None:
                    entry.update(status="error", chunks=0, error="!embed_failed")
                else:
                    rows.extend(one)
        self.store.save_files(group.entries, rows)
        self._last_work = time.monotonic()

    def _embed(self, group: "_Group") -> list[dict] | None:
        """Embed a group, waiting out GPU memory shortages. None if the group itself fails."""
        if not any(group.chunks):
            return []  # only skipped files: no need to touch the GPU
        attempts = 0
        self._embedding = True
        try:
            self._ensure_model()
            while True:
                self._ensure_gpu_for_work()
                vram = self.governor.metrics.vram_free_gb
                self.embedder.batch_scale = (
                    1.0 if vram is None or vram >= 2.0 else 0.5 if vram >= 1.0 else 0.25
                )
                try:
                    rows = group.embed(self.embedder)
                except GpuBusy:
                    attempts += 1
                    self._update(note={"key": "note.vram_full", "params": {}})
                    self.embedder.free_cache()
                    if attempts >= 3:
                        self.embedder.release_gpu("GPU memory full")
                    time.sleep(min(10 * attempts, 60))
                    continue
                except Exception:
                    log.exception("Embedding failed")
                    rows = None
                self._update(note=None)
                return rows
        finally:
            self._embedding = False


class _Group:
    """Files whose chunks are embedded and saved together."""

    def __init__(self):
        self.entries: list[dict] = []
        self.chunks: list[list] = []
        self.n_text = 0
        self.n_image = 0

    def add(self, entry: dict, chunks: list):
        entry["chunks"] = len(chunks)
        self.entries.append(entry)
        self.chunks.append(chunks)
        for c in chunks:
            if c.image is not None:
                self.n_image += 1
            else:
                self.n_text += 1

    def full(self, scale: float = 1.0) -> bool:
        return (self.n_text >= config.GROUP_CHUNKS * scale
                or self.n_image >= max(4, int(32 * scale)))

    def embed(self, embedder: Embedder) -> list[dict]:
        texts, images, meta_t, meta_i = [], [], [], []
        for entry, chunks in zip(self.entries, self.chunks):
            for i, c in enumerate(chunks):
                if c.image is not None:
                    images.append(c.image)
                    meta_i.append((entry, i, c.snippet))
                else:
                    texts.append(c.text)
                    meta_t.append((entry, i, c.snippet))
        rows = []
        for vecs, meta in ((embedder.documents(texts), meta_t), (embedder.images(images), meta_i)):
            for v, (entry, i, snippet) in zip(vecs, meta):
                rows.append({
                    "path": entry["path"], "folder_id": entry["folder_id"], "chunk": i,
                    "kind": entry["kind"], "text": snippet, "vector": v.tolist(),
                })
        return rows


class _Containers:
    """Looking into archives and emails while walking a folder. One that hasn't changed since the
    last scan isn't opened again: what's inside it is known, and so is why the rest isn't searched
    (opening every email at every start would take minutes in a big mail folder)."""

    def __init__(self, types: Types, saved: dict | None = None, members: dict | None = None):
        self.types = types
        self.fingerprint = types.fingerprint()
        self.saved = saved or {}  # normcase(path) -> what the last scan found
        self.members = members or {}  # normcase(path) -> [(path, size, mtime)] known inside it
        self.seen: dict[str, dict] = {}  # normcase(path) -> what this scan found

    def files(self, path: str, size: int, mtime: float, report: Report):
        key = os.path.normcase(path)
        old = self.saved.get(key)
        if old and old["size"] == size and abs(old["mtime"] - mtime) <= 1e-3 and old["types"] == self.fingerprint:
            report.merge(old["report"])
            self.seen[key] = old
            yield from self.members.get(key, ())
            return
        types, found = self.types, Report()
        for item in archive.walk(path, mtime, types.inside, types.rules["nested_archives"], types.max_entries):
            if item.problem:
                found.add(item.problem, item.path)
                continue
            why = skipped_part(types, item.path[len(path) + 1:].split("\\"))
            if why:
                found.add(why, item.path)
            elif types.kind(item.name):
                yield item.path, item.size, item.mtime
            elif not types.inside(item.name):
                found.add_type(types.why_not(item.name), item.path)
        report.merge(found.data)
        self.seen[key] = {"path": path, "size": size, "mtime": mtime, "types": self.fingerprint, "report": found.data}


class Watcher(FileSystemEventHandler):
    """Collects file-system events and hands them to the indexer after they settle."""

    DEBOUNCE_S = 2.0

    def __init__(self, indexer: Indexer):
        self.indexer = indexer
        self.observer = Observer()
        self.observer.daemon = True
        self.observer.start()
        self.watches: dict[int, object] = {}
        self._pending: dict[str, float] = {}
        self._lock = threading.Lock()
        threading.Thread(target=self._drain, name="watcher", daemon=True).start()

    def sync(self):
        wanted = {f["id"]: f["path"] for f in self.indexer.store.folders() if f["watch"]}
        for folder_id in list(self.watches):
            if folder_id not in wanted:
                self.observer.unschedule(self.watches.pop(folder_id))
        for folder_id, path in wanted.items():
            if folder_id not in self.watches and os.path.isdir(path):
                try:
                    self.watches[folder_id] = self.observer.schedule(self, path, recursive=True)
                except OSError:
                    log.exception("Cannot watch %s", path)

    def on_any_event(self, event):
        if event.event_type not in ("created", "modified", "deleted", "moved"):
            return
        if event.is_directory and event.event_type == "modified":
            return  # fires whenever a child changes; the child event covers it
        now = time.monotonic()
        with self._lock:
            for p in (event.src_path, getattr(event, "dest_path", "")):
                if p:
                    self._pending[os.fsdecode(p)] = now

    def _drain(self):
        while True:
            time.sleep(1)
            now = time.monotonic()
            with self._lock:
                ready = [p for p, t in self._pending.items() if now - t >= self.DEBOUNCE_S]
                for p in ready:
                    del self._pending[p]
            try:
                self.indexer.enqueue_paths(ready)
            except Exception:
                log.exception("Could not note %d changed paths", len(ready))
