"""Local web UI: FastAPI server on 127.0.0.1, shown in the desktop window or a browser."""

import argparse
import contextlib
import dataclasses
import json
import logging
import os
import socket
import subprocess
import threading
import webbrowser
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")

import numpy as np
import psutil
import uvicorn
from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, HTMLResponse, JSONResponse, PlainTextResponse, Response
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import archive, config, filetypes, i18n, shortcuts
from .embedder import Embedder
from .extract import has_preview, max_bytes, picture
from .filemap import FileMap
from .filetypes import Types
from .governor import MODES, Governor, lean
from .indexer import Indexer, is_inside
from .search import KINDS, fts_query, fuse, query_terms
from .store import Settings, Store
from .updater import REPO_URL, UpdateProblem, Updater

log = logging.getLogger(__name__)
STATIC = Path(__file__).parent / "static"

# Only the app's own files may run or load in the page.
SECURITY_HEADERS = {
    "Content-Security-Policy": (
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
        "connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    ),
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "X-Frame-Options": "DENY",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Cross-Origin-Opener-Policy": "same-origin",
}
# Opening these would run them (or, for .reg, change Windows): they open in an editor instead.
# Web pages are fine: the browser keeps them in its sandbox.
RUNNABLE_EXTS = {
    ".bat", ".cmd", ".ps1", ".psm1", ".psd1", ".js", ".mjs", ".cjs", ".jse", ".vbs", ".vbe", ".wsf",
    ".wsh", ".py", ".pyw", ".sh", ".bash", ".zsh", ".fish", ".lua", ".rb", ".php", ".pl", ".r", ".hta",
    ".reg", ".ahk", ".au3", ".tcl",
}


class FolderIn(BaseModel):
    path: str


class WatchIn(BaseModel):
    watch: bool


class ExampleIn(BaseModel):
    reason: str
    index: int


class SettingsIn(BaseModel):
    perf_mode: str | None = None
    language: str | None = None
    theme: str | None = None
    check_updates: bool | None = None
    autostart: bool | None = None
    view_files: str | None = None
    view_images: str | None = None
    # Settings -> File types
    types_off: list[str] | None = None
    types_added: list[str] | None = None
    attachments: bool | None = None
    nested_archives: bool | None = None
    big_archives: bool | None = None
    hidden_files: bool | None = None
    program_folders: bool | None = None


class ThemeIn(BaseModel):
    dark: bool


THEMES = ("auto", "light", "dark")
VIEWS = ("cards", "list", "grid")  # how results are shown: with passages, one line each, thumbnails


def fail(status: int, code: str, **params):
    """An error the UI can translate: an i18n key, its parameters, and English for logs."""
    key = f"err.{code}"
    raise HTTPException(status, {"key": key, "params": params, "message": i18n.t("en", key, **params)})


def tk_pick_folder(title: str) -> str | None:
    """Native folder picker for browser mode (the desktop app uses its own window's dialog)."""
    import tkinter as tk
    from tkinter import filedialog

    root = tk.Tk()
    root.withdraw()
    root.attributes("-topmost", True)
    path = filedialog.askdirectory(parent=root, title=title, mustexist=True)
    root.destroy()
    return path or None


@dataclass
class Hooks:
    """What differs between browser mode and the desktop app."""

    app_mode: bool = False
    pick_folder: Callable[[str], str | None] = tk_pick_folder
    show_window: Callable[[], None] | None = None
    quit_app: Callable[[], None] | None = None  # desktop app: exit so an update can finish
    relaunch: str | None = None  # desktop app: what to start again after an update
    set_dark: Callable[[bool], None] | None = None  # desktop app: title bar in the page's theme


@dataclass
class Backend:
    store: Store
    settings: Settings
    governor: Governor
    embedder: Embedder
    indexer: Indexer
    updater: Updater


def create_app(b: Backend, hooks: Hooks) -> FastAPI:
    store, settings, governor, embedder, indexer, updater = (
        b.store, b.settings, b.governor, b.embedder, b.indexer, b.updater
    )
    app = FastAPI(title="Quiekel Embed", docs_url=None, redoc_url=None, openapi_url=None)
    allowed_hosts = {f"127.0.0.1:{config.PORT}", f"localhost:{config.PORT}"}
    pick_lock = threading.Lock()
    update_lock = threading.Lock()
    maps = FileMap(store)

    @app.middleware("http")
    async def local_only(request: Request, call_next):
        # Blocks DNS rebinding (Host), other web pages (Sec-Fetch-Site, which browsers
        # always send and pages can't fake), and form posts from them (custom header).
        if request.headers.get("host") not in allowed_hosts:
            return PlainTextResponse("Forbidden", status_code=403)
        if request.headers.get("sec-fetch-site", "same-origin") not in ("same-origin", "none"):
            return PlainTextResponse("Forbidden", status_code=403)
        if request.method not in ("GET", "HEAD") and request.headers.get("x-quiekel-embed") != "1":
            return PlainTextResponse("Forbidden", status_code=403)
        response = await call_next(request)
        response.headers.update(SECURITY_HEADERS)
        if request.url.path.startswith("/static/"):
            response.headers["Cache-Control"] = "no-cache"  # revalidate, so updates show up
        return response

    @app.get("/")
    def index():
        # The saved theme goes into the page, so static/theme.js can pick light or dark before
        # anything is drawn: no white flash when the app opens in dark mode.
        mode = settings.get("theme")
        page = (STATIC / "index.html").read_text(encoding="utf-8")
        page = page.replace("<html ", f'<html data-theme-mode="{mode if mode in THEMES else "auto"}" ', 1)
        return HTMLResponse(page, headers={"Cache-Control": "no-cache"})

    # ---- status & settings ----------------------------------------------

    def lang() -> str:
        return i18n.resolve(settings.get("language"))

    me = psutil.Process()

    def memory_view() -> dict:
        """What the app holds in memory, for the Performance setting's explanation."""
        vectors = store.vectors
        return {"app_mb": round(me.memory_info().rss / 1e6),
                "vectors_mb": round(vectors.rows * config.EMBED_DIM * 4 / 1e6, 1) if vectors.ready else 0}

    def settings_view() -> dict:
        return {
            **settings.all(),
            "modes": list(MODES),
            "autostart": shortcuts.autostart_enabled() if hooks.app_mode else None,
            "language_resolved": lang(),
            "language_system": i18n.system_language(),
            "languages": i18n.languages(),
        }

    @app.get("/api/status")
    def status():
        return {
            "model": {
                "id": config.MODEL_ID, "status": embedder.status, "error": embedder.error,
                "on_gpu": embedder.on_gpu, "cuda": embedder.cuda, "gpu_name": embedder.gpu_name,
                "engine": embedder.engine,
                "dim": config.EMBED_DIM,
            },
            "progress": indexer.progress(),
            "resources": {
                "decision": dataclasses.asdict(governor.decision),
                "metrics": dataclasses.asdict(governor.metrics),
            },
            "settings": settings_view(),
            "memory": memory_view(),
            "folders": store.folders(),
            "update": {**updater.status(), "can_apply": hooks.app_mode and updater.git_checkout},
            "app_mode": hooks.app_mode,
        }

    @app.post("/api/settings")
    def update_settings(body: SettingsIn):
        if body.perf_mode is not None:
            if body.perf_mode not in MODES:
                fail(400, "unknown_mode", mode=body.perf_mode)
            was_lean = lean(settings.get("perf_mode"))
            settings.update(perf_mode=body.perf_mode)
            governor.refresh()  # apply right away
            if lean(body.perf_mode) != was_lean:  # Light keeps the search data on disk
                store.keep_vectors_in_memory(not lean(body.perf_mode))
        if body.language is not None:
            if body.language != "auto" and body.language not in i18n.languages():
                fail(400, "unknown_language", lang=body.language)
            settings.update(language=body.language)
        if body.theme is not None:
            if body.theme not in THEMES:
                fail(400, "unknown_theme", theme=body.theme)
            settings.update(theme=body.theme)
        if body.check_updates is not None:
            settings.update(check_updates=body.check_updates)
        for key in ("view_files", "view_images"):
            view = getattr(body, key)
            if view is not None:
                if view not in VIEWS:
                    fail(400, "unknown_view", view=view)
                settings.update(**{key: view})
        before = Types.of(settings).fingerprint()
        for key in ("types_off", "types_added"):
            values = getattr(body, key)
            if values is not None:
                if len(values) > 500:
                    fail(400, "too_many_types")
                bad = [v for v in values if not filetypes.clean_ext(v)]
                if bad:
                    fail(400, "unknown_type", type=str(bad[0])[:20])
                settings.update(**{key: sorted({filetypes.clean_ext(v) for v in values})})
        for key in filetypes.RULES:
            if getattr(body, key) is not None:
                settings.update(**{key: getattr(body, key)})
        if Types.of(settings).fingerprint() != before:
            for f in store.folders():  # pick up, or forget, what's searched now
                indexer.enqueue_scan(f["id"])
        if body.autostart is not None:
            if not hooks.app_mode:
                fail(400, "autostart_app_only")
            shortcuts.set_autostart(body.autostart)
        return settings_view()

    # ---- updates ----------------------------------------------------------

    @app.post("/api/update/check")
    def check_for_update():
        updater.check()
        return updater.status()

    @app.post("/api/update/apply")
    def apply_update():
        if not (hooks.app_mode and hooks.quit_app):
            fail(400, "update_app_only")
        if not update_lock.acquire(blocking=False):
            fail(409, "update_running")
        try:
            uv = updater.preflight()
        except UpdateProblem as e:
            update_lock.release()
            raise HTTPException(409, {"key": e.key, "params": e.params, "message": str(e)})
        updater.launch_helper(uv, hooks.relaunch)
        threading.Timer(1.0, hooks.quit_app).start()  # let this response reach the page first
        return {"ok": True}

    @app.post("/api/update/open")
    def open_release_page():
        """Open the release notes (or the repository) in the default browser.
        Only these two known addresses can be opened, never one sent by the page."""
        webbrowser.open(updater.latest["url"] if updater.latest else REPO_URL)
        return {"ok": True}

    @app.post("/api/show")
    def show():
        """Bring the desktop window forward (used when the app is launched a second time)."""
        if hooks.show_window is None:
            return {"ok": False}
        hooks.show_window()
        return {"ok": True}

    @app.post("/api/report-bug")
    def report_bug():
        """GitHub's issue form, with the app and Windows version filled in. Nothing else is sent."""
        import platform
        import urllib.parse

        body = ("**What happened?**\n\n\n**What did you expect?**\n\n\n---\n"
                f"Quiekel Embed {updater.status()['current']} on {platform.platform(terse=True)}")
        webbrowser.open(f"{REPO_URL}/issues/new?{urllib.parse.urlencode({'body': body})}")
        return {"ok": True}

    @app.post("/api/window/theme")
    def window_theme(body: ThemeIn):
        """The page tells the desktop window which theme it shows, for the title bar."""
        if hooks.set_dark:
            hooks.set_dark(body.dark)
        return {"ok": True}

    # ---- folders ----------------------------------------------------------

    @app.post("/api/folders")
    def add_folder(body: FolderIn):
        raw = body.path.strip().strip('"')
        if not raw:
            fail(400, "empty_path")
        path = str(Path(raw).expanduser().resolve())
        if not os.path.isdir(path):
            fail(400, "not_a_folder", path=path)
        for f in store.folders():
            if is_inside(path, f["path"]):
                fail(400, "already_covered", path=f["path"])
            if is_inside(f["path"], path):
                fail(400, "contains_folder", path=f["path"])
        folder_id = store.add_folder(path)
        indexer.enqueue_scan(folder_id)
        indexer.watcher.sync()
        return {"id": folder_id, "path": path}

    @app.post("/api/folders/pick")
    def pick_folder():
        """Open the native Windows folder picker on this machine."""
        if not pick_lock.acquire(blocking=False):
            fail(409, "picker_open")
        try:
            path = hooks.pick_folder(i18n.t(lang(), "folders.picker_title"))
            return {"path": str(Path(path)) if path else None}
        finally:
            pick_lock.release()

    @app.delete("/api/folders/{folder_id}")
    def remove_folder(folder_id: int):
        if not store.folder(folder_id):
            fail(404, "unknown_folder")
        indexer.remove_folder(folder_id)
        return {"ok": True}

    @app.post("/api/folders/{folder_id}/rescan")
    def rescan(folder_id: int):
        if not store.folder(folder_id):
            fail(404, "unknown_folder")
        indexer.enqueue_scan(folder_id)  # (also tries the unreadable files again)
        return {"ok": True}

    @app.post("/api/folders/{folder_id}/errors/seen")
    def errors_seen(folder_id: int):
        """The folder's unreadable files were looked at: their warning has done its job."""
        if not store.folder(folder_id):
            fail(404, "unknown_folder")
        store.errors_seen(folder_id)
        return {"ok": True}

    @app.post("/api/folders/{folder_id}/watch")
    def set_watch(folder_id: int, body: WatchIn):
        if not store.folder(folder_id):
            fail(404, "unknown_folder")
        store.set_watch(folder_id, body.watch)
        indexer.watcher.sync()
        if body.watch:
            indexer.enqueue_scan(folder_id)  # pick up what changed while unwatched
        return {"ok": True}

    @app.get("/api/folders/{folder_id}/errors")
    def folder_errors(folder_id: int):
        return store.errors(folder_id)

    @app.get("/api/folders/{folder_id}/details")
    def folder_details(folder_id: int):
        """What isn't searched in a folder, and why: what its last scan left out, the files that
        were skipped, and the ones that couldn't be read."""
        if not store.folder(folder_id):
            fail(404, "unknown_folder")
        return {**store.details(folder_id), "errors": store.errors(folder_id)}

    @app.get("/api/filetypes")
    def file_types():
        """Settings -> File types: every type there is, and what was changed."""
        return filetypes.menu(Types.of(settings))

    @app.post("/api/pause")
    def pause():
        indexer.pause()
        return {"ok": True}

    @app.post("/api/resume")
    def resume():
        indexer.resume()
        return {"ok": True}

    @app.post("/api/retry-model")
    def retry_model():
        indexer.retry_model()
        return {"ok": True}

    # ---- search ----------------------------------------------------------

    def present(results: list[dict]) -> list[dict]:
        files = store.files_by_ids([r["file_id"] for r in results])
        out = []
        for r in results:
            f = files.get(r["file_id"])
            if not f:
                continue
            p = Path(f.path)
            out.append({
                **r, "path": f.path, "name": p.name, "dir": str(p.parent), "kind": f.kind,
                "mtime": f.mtime, "size": f.size,
                "preview": has_preview(p.name, f.kind),
            })
        return out

    def vector_hits(vector: np.ndarray, n: int, kind: str, folder: int | None, under: str | None) -> list[dict]:
        """Nearest chunks, optionally only from files inside the folder `under`."""
        ids = store.file_ids_under(under) if under else None
        return store.search(vector, n, KINDS.get(kind, ()), folder, ids)

    @app.get("/api/search")
    def search(q: str, kind: str = "all", folder: int | None = None, under: str | None = None, limit: int = 30):
        q = q.strip()
        limit = min(limit, 100)
        if not q:
            return {"results": [], "semantic": embedder.ready}
        indexer.wake_model()  # if it went to sleep (Light performance); this search uses keywords
        try:
            query_vec = embedder.query(q) if embedder.ready else None
        except AttributeError:  # it went to sleep this very moment
            query_vec = None
        hits = vector_hits(query_vec, limit * 6, kind, folder, under) if query_vec is not None else []
        match = fts_query(query_terms(q))
        keyword_hits = store.keyword_search(match, limit * 4, KINDS.get(kind, ()), folder, under) if match else []
        similarity_of = (lambda ids: store.similarities(query_vec, ids)) if query_vec is not None else None
        return {
            "results": present(fuse(hits, keyword_hits, q, limit, similarity_of)),
            "semantic": embedder.ready,  # False: keyword matches only, model still loading
        }

    @app.get("/api/dirs")
    def dirs(path: str = ""):
        """Folders to narrow a search to: the indexed folders, or the ones inside `path`."""
        if not path:
            return {"path": "", "dirs": [{"path": f["path"], "name": Path(f["path"]).name or f["path"],
                                          "count": f["indexed"] + f["pending"]} for f in store.folders()]}
        return {"path": path, "dirs": store.subdirs(path)}

    # ---- map -------------------------------------------------------------

    @app.get("/api/map")
    def file_map(request: Request):
        # Kept ready as JSON; the page asks again every few seconds while the map updates,
        # and gets "not modified" until it changed.
        body, tag = maps.reply()
        headers = {"ETag": tag, "Cache-Control": "no-cache"}
        if request.headers.get("if-none-match") == tag:
            return Response(status_code=304, headers=headers)
        return Response(body, media_type="application/json", headers=headers)

    def mean_vector(file_id: int) -> np.ndarray:
        vecs = store.file_vectors(file_id)
        if not len(vecs):
            fail(404, "no_vectors")
        v = vecs.mean(axis=0)
        return v / (np.linalg.norm(v) or 1.0)

    @app.get("/api/map/vector/{file_id}")
    def file_vector(file_id: int):
        """What the map's card shows of a file: its vector (the "fingerprint", the mean of its
        passages), and a preview: a picture for photos and PDFs, else the start of its text."""
        f = store.file(file_id)
        if not f:
            fail(404, "file_not_found")
        return {
            "vector": [round(float(x), 4) for x in mean_vector(file_id)],
            "file": {"mtime": f.mtime, "size": f.size, "kind": f.kind,
                     "preview": has_preview(Path(f.path).name, f.kind),
                     "passage": store.first_passage(file_id)[:600]},
        }

    @app.get("/api/map/related/{file_id}")
    def related(file_id: int):
        """How similar every file on the map is to this one (cosine, in all 256 dimensions)."""
        return Response(json.dumps(maps.related(mean_vector(file_id)), separators=(",", ":")), media_type="application/json")

    @app.get("/api/map/dimension/{index}")
    def dimension(index: int):
        """Every file's value in one of the 256 dimensions, to color the map by it."""
        if not 0 <= index < config.EMBED_DIM:
            fail(404, "file_not_found")
        return Response(json.dumps(maps.dimension(index), separators=(",", ":")), media_type="application/json")

    @app.get("/api/similar/{file_id}")
    def similar(file_id: int, kind: str = "all", folder: int | None = None, under: str | None = None,
                limit: int = 30):
        v = mean_vector(file_id)  # (the files' own vectors: no need for the model)
        hits = [h for h in vector_hits(v, min(limit, 100) * 6, kind, folder, under) if h["file_id"] != file_id]
        return {"results": present(fuse(hits, [], "", min(limit, 100))), "semantic": True}

    # ---- files -----------------------------------------------------------

    def indexed_file(file_id: int):
        f = store.file(file_id)
        if not f or not archive.exists(f.path):
            fail(404, "file_not_found")
        return f

    @contextlib.contextmanager
    def readable(f):
        """The file on disk; for a file inside an archive, a temporary copy of it."""
        if archive.split(f.path) is None:
            yield Path(f.path)
        else:
            with archive.extracted(f.path, max_bytes(f.kind)) as copy:
                yield copy

    def rendered(file_id: int, size: int, quality: int):
        """JPEG preview of an image, or of a document's first page, cached on disk."""
        f = indexed_file(file_id)
        config.THUMBS_DIR.mkdir(parents=True, exist_ok=True)
        out = config.THUMBS_DIR / f"{file_id}-{int(f.mtime)}-{size}.jpg"
        if not out.exists():
            try:
                with readable(f) as path:
                    img = picture(path, size)
                img.save(out, "JPEG", quality=quality)
            except Exception as e:
                log.info("No preview for %s: %s", f.path, e)
                fail(415, "no_preview")
        return FileResponse(out, headers={"Cache-Control": "max-age=86400"})

    @app.get("/api/thumb/{file_id}")
    def thumb(file_id: int):
        return rendered(file_id, 360, 82)

    @app.get("/api/preview/{file_id}")
    def preview(file_id: int):
        return rendered(file_id, 1600, 88)

    @app.post("/api/open/{file_id}")
    def open_file(file_id: int):
        f = indexed_file(file_id)
        path = f.path
        if archive.split(path) is not None:  # inside an archive: open a copy of just that file
            try:
                path = str(archive.open_copy(path, max_bytes(f.kind)))
            except (OSError, ValueError, KeyError):
                fail(404, "file_not_found")
        if Path(path).suffix.lower() in RUNNABLE_EXTS:
            # Scripts and web pages would *run* when opened: show them in an editor instead.
            try:
                os.startfile(path, "edit")
            except OSError:
                subprocess.Popen(["notepad.exe", path])
        else:
            os.startfile(path)
        return {"ok": True}

    @app.post("/api/reveal/{file_id}")
    def reveal_file(file_id: int):
        path = indexed_file(file_id).path
        where = archive.split(path)
        subprocess.Popen(f'explorer /select,"{where[0] if where else path}"')  # an archive: show the archive
        return {"ok": True}

    @app.post("/api/folders/{folder_id}/reveal")
    def reveal_example(folder_id: int, body: ExampleIn):
        """Show in Explorer one of the files a folder's details name for a reason (files the index
        doesn't hold). The page says which one; the path comes from the scan's own report."""
        examples = store.details(folder_id)["report"].get(body.reason, {}).get("examples", [])
        if not 0 <= body.index < len(examples):
            fail(404, "file_not_found")
        where = archive.split(examples[body.index])
        target = where[0] if where else examples[body.index]  # in an archive: show the archive
        if not os.path.exists(target):
            fail(404, "file_not_found")
        subprocess.Popen(f'explorer /select,"{target}"')
        return {"ok": True}

    @app.exception_handler(Exception)
    async def unhandled(request: Request, exc: Exception):
        log.exception("Request failed: %s", request.url.path)
        return JSONResponse({"detail": f"{type(exc).__name__}: {exc}"}, status_code=500)

    app.mount("/static", StaticFiles(directory=STATIC), name="static")
    return app


URL = f"http://{config.HOST}:{config.PORT}"


def port_in_use(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex((config.HOST, port)) == 0


def build_server(hooks: Hooks) -> tuple[uvicorn.Server, Backend]:
    archive.clear_opened()  # copies of files opened from archives last time
    store = Store()
    settings = Settings(store)
    governor = Governor(lambda: settings.get("perf_mode"))
    embedder = Embedder()
    indexer = Indexer(store, embedder, governor, settings)
    updater = Updater(lambda: settings.get("check_updates"))
    backend = Backend(store, settings, governor, embedder, indexer, updater)
    app = create_app(backend, hooks)
    store.keep_vectors_in_memory(not lean(settings.get("perf_mode")))  # read in the background
    indexer.start()
    server = uvicorn.Server(
        uvicorn.Config(app, host=config.HOST, port=config.PORT, log_level="warning",
                       server_header=False)
    )
    return server, backend


def main():
    """Browser mode: `uv run quiekel-embed`. The desktop app lives in desktop.py."""
    parser = argparse.ArgumentParser(description="Semantic search over your own files.")
    parser.add_argument("--no-browser", action="store_true", help="don't open the browser")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    if port_in_use(config.PORT):
        print(f"Quiekel Embed is already running: {URL}")
        if not args.no_browser:
            webbrowser.open(URL)
        return

    server, _ = build_server(Hooks())
    print(f"\n  Quiekel Embed running at {URL}  (Ctrl+C to stop)\n")
    if not args.no_browser:
        threading.Timer(1.0, webbrowser.open, args=(URL,)).start()
    server.run()


if __name__ == "__main__":
    main()
