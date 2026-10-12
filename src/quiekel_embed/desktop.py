"""Desktop app: a native window (Edge WebView2, built into Windows) plus a tray icon.

Closing the window only hides it: indexing and the folder watcher keep running in the
tray until you choose Quit. Launching the app again brings the existing window back.
"""

import argparse
import base64
import ctypes
import hashlib
import html
import json
import logging
import os
import socket
import sys
import threading
import time
import urllib.request
import webbrowser
from pathlib import Path

from . import config, i18n

log = logging.getLogger(__name__)
ASSETS = Path(__file__).resolve().parent / "assets"
STATIC = Path(__file__).resolve().parent / "static"
URL = f"http://{config.HOST}:{config.PORT}"
TITLE = f"Quiekel Embed [{config.COPY_NAME}]" if config.COPY_NAME else "Quiekel Embed"
# Warm paper, like the UI, and its dark version: background, text, muted text.
COLORS = {False: ("#eeefe9", "#151515", "#6b6d65"), True: ("#151618", "#f2f2ee", "#9b9d95")}
MODES = ("off", "gentle", "balanced", "full")


def _redirect_output():
    """pythonw has no console: send prints, logs and download progress to a log file."""
    if sys.stdout is not None and sys.stderr is not None:
        return
    config.DATA_DIR.mkdir(parents=True, exist_ok=True)
    try:
        if config.LOG_PATH.stat().st_size > 5_000_000:  # keep one old log, start fresh
            config.LOG_PATH.replace(config.LOG_PATH.with_suffix(".log.1"))
    except OSError:
        pass
    f = open(config.LOG_PATH, "a", encoding="utf-8", buffering=1)
    sys.stdout = sys.stdout or f
    sys.stderr = sys.stderr or f


def _style_title_bar(hwnd: int, dark: bool = False):
    """Title bar in the app's colours on Windows 11 (silently ignored on older versions)."""
    # COLORREF is 0x00BBGGRR: caption like the sidebar, text, and the window border.
    caption, text, border = (0x001F1C1B, 0x00EEF2F2, 0x00363230) if dark else (0x00E0E7E5, 0x00151515, 0x00C9D1D0)
    try:
        dwm = ctypes.windll.dwmapi
        for attr, value in (
            (20, int(dark)),  # DWMWA_USE_IMMERSIVE_DARK_MODE: light or dark caption buttons
            (35, caption),  # DWMWA_CAPTION_COLOR
            (36, text),  # DWMWA_TEXT_COLOR
            (34, border),  # DWMWA_BORDER_COLOR
        ):
            v = ctypes.c_int(value)
            dwm.DwmSetWindowAttribute(ctypes.c_void_p(hwnd), attr, ctypes.byref(v), ctypes.sizeof(v))
        # Redraw the frame now (SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED).
        ctypes.windll.user32.SetWindowPos(ctypes.c_void_p(hwnd), None, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0004 | 0x0020)
    except (AttributeError, OSError):
        pass


def _starts_dark() -> bool:
    """The theme to open in: the saved setting, or the Windows app theme for "auto"."""
    theme = "auto"
    try:
        import sqlite3

        db = sqlite3.connect(f"{config.DB_PATH.as_uri()}?mode=ro", uri=True)
        try:
            row = db.execute("SELECT value FROM meta WHERE key = 'setting.theme'").fetchone()
        finally:
            db.close()
        theme = json.loads(row[0]) if row else "auto"
    except Exception:
        pass
    if theme in ("light", "dark"):
        return theme == "dark"
    try:
        import winreg

        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize") as key:
            return winreg.QueryValueEx(key, "AppsUseLightTheme")[0] == 0
    except OSError:
        return False


_INSTANCE: list = []  # the handle below, held for as long as the app runs


def _first_copy() -> bool:
    """Whether this is the app's only copy in this Windows session. A named mutex exists from the
    first moment; the server's port only once the app has started, which takes a few seconds. A
    copy with a data folder of its own has a mutex of its own."""
    name = config.APP_ID
    if config.COPY_NAME:
        name += "." + hashlib.sha1(str(config.DATA_DIR).lower().encode()).hexdigest()[:12]
    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateMutexW.restype = ctypes.c_void_p
        handle = kernel32.CreateMutexW(None, False, f"Local\\{name}")
        already = ctypes.get_last_error() == 183  # ERROR_ALREADY_EXISTS
    except (AttributeError, OSError):
        return True
    _INSTANCE.append(handle)
    return not already


def _port_in_use() -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex((config.HOST, config.PORT)) == 0


def _show_running_instance() -> bool:
    try:
        # Let the running instance take focus: this launch is the user's own action.
        ctypes.windll.user32.AllowSetForegroundWindow(-1)
    except (AttributeError, OSError):
        pass
    req = urllib.request.Request(f"{URL}/api/show", method="POST", headers={"x-quiekel-embed": "1"})
    try:
        with urllib.request.urlopen(req, timeout=3) as r:
            return bool(json.load(r).get("ok"))
    except (OSError, ValueError):
        return False


def _page(message: str = "", detail: str = "", dark: bool = False) -> str:
    """What the window shows until the app is there: the pig and a ring that fills up as the app
    starts (like the match ring: red to green; progress() moves it), or what went wrong."""
    pig = base64.b64encode((STATIC / ("pig-dark.svg" if dark else "pig.svg")).read_bytes()).decode()
    BG, FG, MUTED = COLORS[dark]
    track, sat, light = ("#2b2c30", 62, 56) if dark else ("#d0d1c9", 72, 44)
    ring = ('<svg class="ring" viewBox="0 0 20 20" aria-hidden="true"><circle class="t" cx="10" cy="10" r="8"/>'
            '<circle class="a" id="a" cx="10" cy="10" r="8" pathLength="100"/></svg>')
    body = f"<p>{html.escape(message)}</p>" if message else ring
    if detail:
        body += f"<p><code>{html.escape(detail)}</code></p>"
    return f"""<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;height:100%;background:{BG};color:{FG};font:14px "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif}}
body{{display:grid;place-items:center}} .box{{display:grid;justify-items:center;gap:20px;max-width:560px;padding:24px;text-align:center}}
img{{width:88px;height:auto}} p{{margin:0;font-weight:600}} code{{font:12px "Cascadia Mono",Consolas,monospace;color:{MUTED}}}
.ring{{width:40px;height:40px;transform:rotate(-90deg)}} .ring circle{{fill:none;stroke-width:2.8}} .t{{stroke:{track}}}
.a{{stroke-linecap:round;stroke-dasharray:4 100;stroke:hsl(4 {sat}% {light}%);transition:stroke-dasharray .4s ease,stroke .4s ease}}
</style><script>
function progress(share) {{
  const a = document.getElementById("a");
  if (!a) return;
  share = Math.max(.04, Math.min(1, share));
  a.style.strokeDasharray = (share * 100).toFixed(1) + " 100";
  a.style.stroke = "hsl(" + Math.round(share * 120) + " {sat}% {light}%)";
}}
</script></head><body><div class="box"><img src="data:image/svg+xml;base64,{pig}" alt="">{body}</div></body></html>"""


class Tray:
    def __init__(self, on_open, on_quit):
        import pystray
        from PIL import Image

        from . import shortcuts

        self.backend = None
        self.lang = i18n.stored_language()  # until the backend (and its settings) is up
        self._shortcuts = shortcuts
        self._hint_flag = config.DATA_DIR / ".tray-hint-shown"
        item = pystray.MenuItem
        ready = lambda _: self.backend is not None  # noqa: E731
        tr = self.tr
        self.icon = pystray.Icon(
            "QuiekelEmbed",
            Image.open(ASSETS / "icon.png"),
            TITLE,
            menu=pystray.Menu(
                item(lambda _: tr("tray.open"), lambda: on_open(), default=True),
                item(
                    lambda _: tr("tray.update", version=self._update_version()),
                    lambda: on_open(),
                    visible=lambda _: self._update_version() is not None,
                ),
                pystray.Menu.SEPARATOR,
                item(lambda _: tr("tray.pause"), self._toggle_pause, checked=lambda _: self._paused(), enabled=ready),
                item(lambda _: tr("tray.speed"), pystray.Menu(*[
                    item(lambda _, m=mode: tr(f"mode.{m}"), self._set_mode(mode), radio=True, enabled=ready,
                         checked=lambda _, m=mode: self._mode() == m)
                    for mode in MODES
                ])),
                item(lambda _: tr("tray.autostart"), self._toggle_autostart,
                     checked=lambda _: shortcuts.autostart_enabled()),
                pystray.Menu.SEPARATOR,
                item(lambda _: tr("tray.quit"), lambda: on_quit()),
            ),
        )
        threading.Thread(target=self.icon.run, name="tray", daemon=True).start()
        threading.Thread(target=self._keep_title_fresh, name="tray-title", daemon=True).start()

    def tr(self, key: str, /, **params) -> str:
        """Translate into the current language (the setting can change while running)."""
        if self.backend:
            self.lang = i18n.resolve(self.backend.settings.get("language"))
        return i18n.t(self.lang, key, **params)

    def _paused(self) -> bool:
        return bool(self.backend and self.backend.indexer.paused)

    def _update_version(self) -> str | None:
        latest = self.backend.updater.latest if self.backend else None
        return latest["version"] if latest else None

    def notify_update(self, release: dict):
        try:
            self.icon.notify(
                self.tr("tray.update_found", version=release["version"]),
                TITLE,
            )
            self.icon.update_menu()
        except Exception:
            log.exception("Tray notification failed")

    def _mode(self) -> str | None:
        return self.backend.settings.get("perf_mode") if self.backend else None

    def _set_mode(self, mode: str):
        def apply():
            if self.backend:
                self.backend.set_perf_mode(mode)
                self.icon.update_menu()
        return apply

    def _toggle_pause(self):
        if not self.backend:
            return
        indexer = self.backend.indexer
        if indexer.paused:
            indexer.resume()
        else:
            indexer.pause()
        self.icon.update_menu()

    def _toggle_autostart(self):
        try:
            self._shortcuts.set_autostart(not self._shortcuts.autostart_enabled())
        except Exception:
            log.exception("Could not change autostart")
        self.icon.update_menu()

    def _keep_title_fresh(self):
        last = ""
        while True:
            time.sleep(2)
            if not self.backend:
                continue
            p = self.backend.indexer.progress()
            d = self.backend.governor.decision
            working = p.get("state") in ("indexing", "scanning")
            if p.get("paused"):
                text = self.tr("tray.title.paused")
            elif p.get("state") == "loading-model":
                text = self.tr("tray.title.starting")
            elif working and (d.duty == 0 or p.get("note")):
                note = p.get("note")
                reason = self.tr(note["key"], **note["params"]) if note else self.tr("gov." + d.code, **d.params)
                text = self.tr("tray.title.waiting", reason=reason)
            elif working:
                name = Path(p.get("folder", "")).name
                pct = f" {p['done'] * 100 // p['total']} %" if p.get("total") else ""
                text = self.tr("tray.title.indexing", name=name, pct=pct)
                if d.level == "throttled":
                    text += f" ({self.tr('gov.' + d.code, **d.params)})"
            else:
                text = self.tr("tray.title.idle")
            if config.COPY_NAME:
                text = f"[{config.COPY_NAME}] {text}"
            if text != last:
                self.icon.title = text[:120]
                last = text

    def hint_once(self):
        """The first time the window is closed, explain where the app went."""
        if self._hint_flag.exists():
            return
        try:
            self.icon.notify(
                self.tr("tray.hint"),
                TITLE,
            )
            self._hint_flag.touch()
        except Exception:
            log.exception("Tray notification failed")

    def stop(self):
        try:
            self.icon.stop()
        except Exception:
            pass


def main():
    parser = argparse.ArgumentParser(description="Quiekel Embed desktop app.")
    parser.add_argument("--hidden", action="store_true", help="start in the tray without a window")
    args = parser.parse_args()

    _redirect_output()
    logging.basicConfig(
        level=logging.INFO, stream=sys.stderr,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    log.info("Desktop app starting")
    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    try:
        # Own taskbar identity: Windows groups the window with the Start Menu shortcut that
        # carries the same ID and shows its pig icon and name, not Python's.
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(config.APP_ID)
    except (AttributeError, OSError):
        pass

    if not _first_copy():
        # Running already, maybe still starting: show that window instead (on a slow PC its server
        # takes a while to answer). Started with Windows: nothing to show.
        log.info("Already running: showing that window")
        deadline = time.monotonic() + 30
        while not args.hidden and not _show_running_instance() and time.monotonic() < deadline:
            time.sleep(0.5)
        return
    if _port_in_use():
        if not _show_running_instance():
            webbrowser.open(URL)  # the running copy is the browser version
        return

    import webview

    dark = {"now": _starts_dark()}  # the page reports changes; the title bar follows
    # WebView2's own background until a page paints, also for each new page's process: never white.
    os.environ["WEBVIEW2_DEFAULT_BACKGROUND_COLOR"] = "FF" + COLORS[dark["now"]][0].lstrip("#").upper()
    window = webview.create_window(
        TITLE,
        html=_page(dark=dark["now"]),
        width=1280, height=840, min_size=(760, 540),
        hidden=args.hidden,
        background_color=COLORS[dark["now"]][0],
        text_select=True,
    )
    quitting = threading.Event()
    styled = {"dark": None}

    def set_dark(on: bool):
        dark["now"] = on
        if styled["dark"] == on:
            return  # (redrawing the frame for nothing could flash it)
        try:
            _style_title_bar(window.native.Handle.ToInt64(), on)
            styled["dark"] = on
        except Exception:
            log.exception("Could not style the title bar")

    def show():
        window.show()
        try:
            hwnd = window.native.Handle.ToInt64()
            if ctypes.windll.user32.IsIconic(hwnd):
                window.restore()
            ctypes.windll.user32.SetForegroundWindow(hwnd)
        except Exception:
            pass

    def quit_app():
        quitting.set()
        window.destroy()

    def on_closing():
        if quitting.is_set():
            return True
        threading.Thread(target=window.hide, daemon=True).start()
        tray.hint_once()
        return False  # cancel the close: keep running in the tray

    def on_before_show():
        set_dark(dark["now"])  # the title bar in the app's colours before the window first shows

    window.events.closing += on_closing
    window.events.before_show += on_before_show
    window.events.shown += on_before_show  # (in case the first try came too early)
    tray = Tray(show, quit_app)

    def progress(share: float):
        """How far the start is, on the splash's ring."""
        try:
            window.evaluate_js(f"progress({share:.2f})")
        except Exception:
            pass  # (the splash isn't there yet, or it's gone)

    def boot():
        # Heavy imports happen here, after the window is already on screen.
        log.info("Window shown, starting server")
        progress(0.12)
        try:
            from . import shortcuts
            from .app import Hooks, build_server, start_background

            progress(0.45)

            def pick_folder(title: str):
                picked = window.create_file_dialog(webview.FileDialog.FOLDER)
                return picked[0] if picked else None

            server, backend = build_server(Hooks(
                app_mode=True, pick_folder=pick_folder, show_window=show,
                quit_app=quit_app, relaunch=str(shortcuts.launcher()), set_dark=set_dark,
            ))
            backend.updater.on_found = tray.notify_update
            tray.backend = backend
            progress(0.75)
            serving = threading.Thread(target=server.run, name="server", daemon=True)
            serving.start()
            deadline = time.monotonic() + 60
            while not server.started:
                if not serving.is_alive() or time.monotonic() > deadline:  # (e.g. its port taken)
                    raise RuntimeError("the local server did not start")
                time.sleep(0.05)
            start_background(backend)
            progress(1)
            time.sleep(0.35)  # (the ring fills up before the app replaces it)
            window.load_url(URL)
            log.info("Server up, UI loaded")
        except Exception as e:
            log.exception("Startup failed")
            window.load_html(_page(
                i18n.t(tray.lang, "splash.failed", error=f"{type(e).__name__}: {e}"),
                i18n.t(tray.lang, "splash.details", path=config.LOG_PATH), dark=dark["now"],
            ))

    webview.start(boot, icon=str(ASSETS / "icon.ico"), private_mode=True)
    tray.stop()
    os._exit(0)  # stop the server, watcher and indexer threads right away


if __name__ == "__main__":
    main()
