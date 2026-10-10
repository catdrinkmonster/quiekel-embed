"""Files inside ZIP archives, searched like files in a folder, without unpacking anything.

A file inside an archive gets a path through the archive, as if the archive were a folder:
C:\\Docs\\photos.zip\\2019\\beach.jpg. Indexing reads that one entry (with limits, because an
archive can lie about its sizes), and opening a result copies just that file to a temp folder.
Nothing is ever written next to your files, and archives inside archives are left alone.
"""

import contextlib
import hashlib
import os
import shutil
import tempfile
import time
import zipfile
from collections.abc import Iterator
from pathlib import Path

SUFFIX = ".zip"
MAX_ENTRIES = 20_000  # bigger archives are backups more than documents: left alone
OPENED = Path(tempfile.gettempdir()) / "Quiekel Embed"  # copies of opened files


def is_archive(path: str) -> bool:
    return path.lower().endswith(SUFFIX)


def split(path: str) -> tuple[str, str] | None:
    """(archive, entry name) for a path through an archive, None for an ordinary path."""
    lower = path.lower()
    i = lower.find(SUFFIX + "\\")
    while i != -1:
        archive = path[: i + len(SUFFIX)]
        if os.path.isfile(archive):
            return archive, path[i + len(SUFFIX) + 1:].replace("\\", "/")
        i = lower.find(SUFFIX + "\\", i + 1)
    return None


def exists(path: str) -> bool:
    """True for files on disk and for paths into an archive that exists."""
    return os.path.exists(path) or split(path) is not None


def entries(archive: str) -> Iterator[tuple[str, list[str], int, float]]:
    """(path through the archive, its parts, size, modification time) for each file inside."""
    try:
        with zipfile.ZipFile(archive) as z:
            infos = z.infolist()
    except (OSError, zipfile.BadZipFile, ValueError):
        return
    if len(infos) > MAX_ENTRIES:
        return
    for info in infos:
        if info.is_dir() or info.flag_bits & 0x1:  # folders, encrypted entries
            continue
        parts = [p for p in info.filename.replace("\\", "/").split("/") if p]
        if not parts or any(p in (".", "..") or ":" in p for p in parts):  # odd or malicious names
            continue
        try:
            mtime = time.mktime((*info.date_time, 0, 0, -1))
        except (OverflowError, ValueError):
            mtime = 0.0
        yield archive + "\\" + "\\".join(parts), parts, info.file_size, mtime


def read(path: str, limit: int) -> bytes:
    """One entry's bytes. Refuses more than `limit`, whatever the archive claims."""
    where = split(path)
    if where is None:
        raise FileNotFoundError(path)
    archive, name = where
    with zipfile.ZipFile(archive) as z:
        info = z.getinfo(name)
        if info.file_size > limit:
            raise ValueError("too large")
        with z.open(info) as f:
            data = f.read(limit + 1)
    if len(data) > limit:
        raise ValueError("too large")
    return data


@contextlib.contextmanager
def extracted(path: str, limit: int) -> Iterator[Path]:
    """The entry as a temporary file with the same name, removed again afterwards."""
    tmp = Path(tempfile.mkdtemp(prefix="quiekel-"))
    try:
        file = tmp / Path(path).name
        file.write_bytes(read(path, limit))
        yield file
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def open_copy(path: str, limit: int) -> Path:
    """A copy of the entry to open in its app (which needs a real file), in a temp folder."""
    archive, name = split(path) or (None, None)
    if archive is None:
        raise FileNotFoundError(path)
    folder = OPENED / f"{Path(archive).stem}-{hashlib.sha1(archive.lower().encode()).hexdigest()[:8]}"
    target = (folder / name).resolve()
    if not target.is_relative_to(folder.resolve()):  # never outside our folder
        raise ValueError("bad name")
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(read(path, limit))
    return target


def clear_opened():
    """Copies of opened files from earlier sessions are no longer needed."""
    shutil.rmtree(OPENED, ignore_errors=True)
