"""Update checks against the project's GitHub releases, and safe one-click updates.

Checking only talks to api.github.com over HTTPS (first a minute after start, then
every 6 hours, and only while enabled in the settings).

Updating is only offered for a clean git checkout of the official repository, on
`main`, and only to a release tag that is part of the protected `main` branch. The
update itself (fast-forward to the tag, then installing the exact, hash-locked
dependencies) runs in a small helper after the app has exited, because Windows
can't replace files the running app has open. If any step fails, the helper
rolls back to the previous commit, and the app starts again either way.
"""

import json
import logging
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path

from . import config, i18n

log = logging.getLogger(__name__)

REPO = "catdrinkmonster/quiekel-embed"
REPO_URL = f"https://github.com/{REPO}"
API_LATEST = f"https://api.github.com/repos/{REPO}/releases/latest"
ALLOWED_REMOTES = {REPO_URL, f"{REPO_URL}.git", f"git@github.com:{REPO}.git"}
ROOT = Path(__file__).resolve().parents[2]
RESULT_FILE = config.DATA_DIR / "update-result.json"
FIRST_CHECK_AFTER_S = 60
CHECK_EVERY_S = 6 * 3600
_NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)
_VERSION_RE = re.compile(r"v?(\d+)\.(\d+)\.(\d+)")
_TAG_RE = re.compile(r"v\d+\.\d+\.\d+")  # release tags are untrusted input: strict shape only


class UpdateProblem(Exception):
    """Why this copy can't update itself: an i18n key under "upd." plus parameters."""

    def __init__(self, code: str, **params):
        self.key, self.params = f"upd.{code}", params
        super().__init__(i18n.t("en", self.key, **params))


def current_version() -> str:
    try:
        return version("quiekel-embed")
    except PackageNotFoundError:
        return "0.0.0"


def parse_version(text: str) -> tuple[int, int, int] | None:
    m = _VERSION_RE.fullmatch(text.strip())
    return tuple(int(x) for x in m.groups()) if m else None


def is_newer(candidate: str, current: str) -> bool:
    new, cur = parse_version(candidate), parse_version(current)
    return bool(new and cur and new > cur)


def parse_release(data: dict, current: str) -> dict | None:
    """The release as shown in the app, or None if it isn't a valid, newer, final release."""
    tag = str(data.get("tag_name", ""))
    url = str(data.get("html_url", ""))
    if (data.get("draft") or data.get("prerelease") or not _TAG_RE.fullmatch(tag)
            or not url.startswith(f"{REPO_URL}/releases/") or not is_newer(tag, current)):
        return None
    return {
        "version": tag[1:],
        "tag": tag,
        "title": str(data.get("name") or tag)[:120],
        "notes": str(data.get("body") or "")[:5000],
        "published": str(data.get("published_at") or "")[:10],
        "url": url,
    }


def _git_env() -> dict:
    # Never prompt for credentials: the repository is public.
    return {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GCM_INTERACTIVE": "never"}


def git(*args: str, timeout: int = 60) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(ROOT), *args], capture_output=True, text=True, timeout=timeout,
        env=_git_env(), creationflags=_NO_WINDOW,
    )


def _git_out(*args: str, timeout: int = 60) -> str:
    out = git(*args, timeout=timeout)
    if out.returncode != 0:
        raise UpdateProblem("git_failed", detail=(out.stderr or out.stdout).strip()[:300] or f"git {args[0]}")
    return out.stdout.strip()


def _is_ancestor(older: str, newer: str) -> bool:
    return git("merge-base", "--is-ancestor", older, newer).returncode == 0


class Updater:
    def __init__(self, enabled: Callable[[], bool]):
        self.current = current_version()
        self.latest: dict | None = None
        self.last_check: float | None = None
        self.error: dict | None = None  # {"key", "params"}: the UI translates it
        self.last_result = self._take_result()
        self._enabled = enabled
        self._lock = threading.Lock()
        self.on_found: Callable[[dict], None] | None = None  # e.g. a tray notification
        threading.Thread(target=self._loop, name="updater", daemon=True).start()

    @property
    def git_checkout(self) -> bool:
        return (ROOT / ".git").exists()

    def status(self) -> dict:
        return {
            "current": self.current, "latest": self.latest, "last_check": self.last_check,
            "error": self.error, "git_checkout": self.git_checkout, "last_result": self.last_result,
            "repo_url": REPO_URL,
        }

    def _loop(self):
        time.sleep(FIRST_CHECK_AFTER_S)
        while True:
            if self._enabled():
                self.check()
            time.sleep(CHECK_EVERY_S)

    def check(self):
        with self._lock:
            req = urllib.request.Request(API_LATEST, headers={
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
                "User-Agent": f"quiekel-embed/{self.current}",
            })
            data = None
            try:
                with urllib.request.urlopen(req, timeout=15) as r:
                    data = json.loads(r.read(2_000_000))
                self.error = None
            except urllib.error.HTTPError as e:
                self.error = None if e.code == 404 else {"key": "upd.http_error", "params": {"status": e.code}}  # 404: no release yet
            except (OSError, ValueError) as e:
                self.error = {"key": "upd.unreachable", "params": {}}
                log.info("Update check failed: %s", e)
            self.last_check = time.time()
            if self.error:
                return
            found = parse_release(data, self.current) if isinstance(data, dict) else None
            if found and (not self.latest or self.latest["tag"] != found["tag"]):
                log.info("Update available: %s", found["tag"])
                if self.on_found:
                    self.on_found(found)
            self.latest = found

    # ---- applying ---------------------------------------------------------

    def preflight(self) -> str:
        """Check that this copy can safely update itself to the latest release. Returns uv's path."""
        if not self.latest:
            raise UpdateProblem("no_release")
        tag = self.latest["tag"]
        if not _TAG_RE.fullmatch(tag):
            raise UpdateProblem("bad_tag")
        if not self.git_checkout:
            raise UpdateProblem("not_git")
        remote = _git_out("remote", "get-url", "origin").rstrip("/")
        if remote not in ALLOWED_REMOTES:
            raise UpdateProblem("wrong_remote", remote=remote)
        branch = _git_out("rev-parse", "--abbrev-ref", "HEAD")
        if branch != "main":
            raise UpdateProblem("wrong_branch", branch=branch)
        if _git_out("status", "--porcelain", "--untracked-files=no"):
            raise UpdateProblem("dirty")
        # Fetch exactly main and this one tag. A tag is never overwritten locally (no '+').
        _git_out("fetch", "--no-tags", "origin",
                 "+refs/heads/main:refs/remotes/origin/main", f"refs/tags/{tag}:refs/tags/{tag}",
                 timeout=180)
        if not _is_ancestor(f"refs/tags/{tag}", "refs/remotes/origin/main"):
            raise UpdateProblem("not_on_main")
        if not _is_ancestor("HEAD", f"refs/tags/{tag}"):
            raise UpdateProblem("diverged")
        uv = shutil.which("uv")
        if not uv:
            raise UpdateProblem("no_uv")
        return uv

    def launch_helper(self, uv: str, relaunch: str | None):
        """Start the helper that finishes the update once this process has exited."""
        tag = self.latest["tag"]
        helper = Path(__file__).with_name("update_helper.py")
        python = Path(sys.executable)
        pythonw = python.with_name("pythonw.exe")
        args = [str(pythonw if pythonw.exists() else python), "-I", "-S", str(helper),
                "--wait-pid", str(os.getpid()), "--root", str(ROOT), "--tag", tag,
                "--uv", uv, "--result", str(RESULT_FILE), "--relaunch", relaunch or ""]
        flags = _NO_WINDOW | getattr(subprocess, "DETACHED_PROCESS", 0) \
            | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        subprocess.Popen(args, creationflags=flags, close_fds=True, cwd=str(ROOT))
        log.info("Update helper started for %s", tag)

    @staticmethod
    def _take_result() -> dict | None:
        """The outcome of an update that ran while the app was closed (reported once)."""
        try:
            result = json.loads(RESULT_FILE.read_text(encoding="utf-8"))
            RESULT_FILE.unlink()
            return result
        except (OSError, ValueError):
            return None
