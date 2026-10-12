"""Files inside other files, searched like files in a folder, without unpacking anything:
archives (ZIP, 7z, RAR, TAR, CAB, ISO, …), single compressed files (.gz) and email attachments.

A file inside gets a path through its container, as if the container were a folder:
C:\\Docs\\photos.zip\\2019\\beach.jpg, or C:\\Mail\\offer.msg\\prices.pdf. A container inside a
container is looked into too, once (an email in a ZIP, a ZIP in a ZIP); deeper ones are left
alone, and so are archives with very many files (mostly backups). Whatever is left out comes
with its reason, for the folder's details.

Indexing reads one entry at a time, with limits (an archive can lie about its sizes), and
opening a result copies just that file to a temp folder. Nothing is ever written next to your
files. 7z, RAR, CAB, ISO and LZH are read by Windows' own archive library, the one File
Explorer uses (Windows 11, 2023 and later); ZIP, TAR and .gz by Python itself.
"""

import bz2
import contextlib
import ctypes
import gzip
import hashlib
import io
import lzma
import os
import re
import shutil
import tarfile
import tempfile
import threading
import time
import zipfile
from collections import OrderedDict
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from pathlib import Path

MAX_ENTRIES = 20_000  # bigger archives are mostly backups: left alone (unless Settings say otherwise)
MAX_ENTRIES_BIG = 1_000_000
MAX_NESTED_BYTES = 256 * 1024 * 1024  # a container inside a container is read into memory, up to this
OPENED = Path(tempfile.gettempdir()) / "Quiekel Embed"  # copies of opened files

ZIP_EXTS = {".zip"}
TAR_EXTS = {".tar", ".tgz", ".tbz", ".tbz2", ".txz"}
SINGLE_EXTS = {".gz", ".bz2", ".xz"}  # one compressed file (unless it's a .tar.gz)
LIBRARY_EXTS = {".7z", ".rar", ".cab", ".iso", ".lzh", ".lha", ".zipx", ".xar", ".tzst"}
MAIL_EXTS = {".eml", ".msg"}
ARCHIVE_EXTS = ZIP_EXTS | TAR_EXTS | SINGLE_EXTS | LIBRARY_EXTS
_SPLIT = re.compile(r"\.part\d+\.rar$", re.I)  # one part of a split archive


class TooLarge(ValueError):
    pass


class Encrypted(Exception):
    """Behind a password."""


class NeedsWindows(Exception):
    """This Windows can't open the archive type (Windows' archive library came with Windows 11, 2023)."""


class ArchiveBroken(Exception):
    """Windows' archive library couldn't read it."""


def container_type(name: str) -> str | None:
    """"zip", "tar", "single", "library" (Windows' archive library) or "mail"; None for plain files."""
    lower = name.lower()
    if lower.endswith((".tar.gz", ".tar.bz2", ".tar.xz")):
        return "tar"
    if lower.endswith(".tar.zst"):
        return "library"
    ext = os.path.splitext(lower)[1]
    if ext in ZIP_EXTS:
        return "zip"
    if ext in TAR_EXTS:
        return "tar"
    if ext in SINGLE_EXTS:
        return "single"
    if ext in LIBRARY_EXTS:
        return "library"
    if ext in MAIL_EXTS:
        return "mail"
    return None


def is_container(name: str) -> bool:
    return container_type(name) is not None


@dataclass
class Entry:
    name: str  # as stored
    size: int
    mtime: float = 0.0  # 0: unknown, the container's own time is used
    encrypted: bool = False
    inline: bool = False  # an email's picture shown inside its text


@dataclass
class Item:
    """Something found inside a container: a file, or why something there can't be searched."""

    path: str
    name: str
    size: int
    mtime: float
    problem: str | None = None


def parts_of(name: str) -> list[str] | None:
    """An entry's name as folders and file name; None for odd or malicious names."""
    parts = [p for p in name.replace("\\", "/").split("/") if p and p != "."]
    if not parts or any(p == ".." or ":" in p for p in parts):
        return None
    return parts


# ---- containers ------------------------------------------------------------------------


class _Container:
    def __init__(self):
        self._clean: dict[str, Entry] | None = None

    def entries(self) -> list[Entry]:
        raise NotImplementedError

    def read_entry(self, entry: Entry, limit: int) -> bytes:
        raise NotImplementedError

    def close(self):
        pass

    def find(self, clean: str) -> Entry | None:
        """The entry behind a cleaned-up name ("a/b.txt", as in the paths we hand out)."""
        if self._clean is None:
            self._clean = {}
            for e in self.entries():
                parts = parts_of(e.name)
                if parts:
                    self._clean.setdefault("/".join(parts).lower(), e)
        return self._clean.get(clean.lower())

    def read(self, clean: str, limit: int) -> bytes:
        entry = self.find(clean)
        if entry is None:
            raise FileNotFoundError(clean)
        if entry.encrypted:
            raise Encrypted(clean)
        if entry.size > limit:
            raise TooLarge(clean)
        data = self.read_entry(entry, limit)
        if len(data) > limit:
            raise TooLarge(clean)
        return data


def _oem_encoding() -> str | None:
    """Windows' own ZIP folders store names in the PC's old "OEM" code page (850 for most of
    Western Europe), unless they say they're UTF-8."""
    try:
        name = f"cp{ctypes.windll.kernel32.GetOEMCP()}"
        "".encode(name)
        return name
    except (AttributeError, OSError, LookupError):
        return None


_OEM = _oem_encoding()


class _Zip(_Container):
    def __init__(self, src):
        super().__init__()
        self.z = zipfile.ZipFile(io.BytesIO(src) if isinstance(src, bytes) else src, metadata_encoding=_OEM)

    def entries(self):
        out = []
        for info in self.z.infolist():
            if info.is_dir():
                continue
            try:
                mtime = time.mktime((*info.date_time, 0, 0, -1))
            except (OverflowError, ValueError):
                mtime = 0.0
            out.append(Entry(info.filename, info.file_size, mtime, encrypted=bool(info.flag_bits & 0x1)))
        return out

    def read_entry(self, entry, limit):
        with self.z.open(entry.name) as f:
            return f.read(limit + 1)

    def close(self):
        self.z.close()


class _Tar(_Container):
    def __init__(self, src):
        super().__init__()
        self.t = tarfile.open(fileobj=io.BytesIO(src)) if isinstance(src, bytes) else tarfile.open(src, "r:*")
        self.members = {m.name: m for m in self.t.getmembers() if m.isreg()}

    def entries(self):
        return [Entry(m.name, m.size, float(m.mtime)) for m in self.members.values()]

    def read_entry(self, entry, limit):
        f = self.t.extractfile(self.members[entry.name])
        return f.read(limit + 1) if f else b""

    def close(self):
        self.t.close()


class _Single(_Container):
    """A single compressed file: notes.txt.gz holds notes.txt."""

    OPEN = {".gz": gzip.GzipFile, ".bz2": bz2.BZ2File, ".xz": lzma.LZMAFile}

    def __init__(self, src, label: str):
        super().__init__()
        self.src, ext = src, os.path.splitext(label.lower())[1]
        self.opener = self.OPEN[ext]
        self.name = os.path.basename(label.replace("\\", "/"))[: -len(ext)] or "file"
        size = 0
        with self._open() as f:  # how big it is unpacked: count (up to a limit)
            while chunk := f.read(1 << 20):
                size += len(chunk)
                if size > MAX_NESTED_BYTES:
                    break
        self.size = size

    def _open(self):
        return self.opener(fileobj=io.BytesIO(self.src)) if isinstance(self.src, bytes) else self.opener(self.src)

    def entries(self):
        return [Entry(self.name, self.size)]

    def read_entry(self, entry, limit):
        with self._open() as f:
            return f.read(limit + 1)


class _Mail(_Container):
    """An email's attachments."""

    def __init__(self, src):
        super().__init__()
        from . import mail

        self.mail = mail.open_mail(src if isinstance(src, bytes) else Path(src).read_bytes())
        self.by_name = {a.name: a for a in self.mail.attachments}

    def entries(self):
        return [Entry(a.name, a.size, inline=a.inline) for a in self.mail.attachments]

    def read_entry(self, entry, limit):
        return self.by_name[entry.name].load()

    def close(self):
        self.mail.close()


# Windows' archive library (libarchive), through ctypes.
_EOF, _WARN = 1, -20
_REGULAR = 0o100000
_dll = None


def _library():
    global _dll
    if _dll is None:
        path = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "archiveint.dll")
        try:
            dll = ctypes.WinDLL(path)  # by its full path: never a DLL of the same name from elsewhere
        except (OSError, AttributeError):
            _dll = False
            return None
        p, c = ctypes.c_void_p, ctypes
        for name, res, args in (
            ("archive_read_new", p, []),
            ("archive_read_support_filter_all", c.c_int, [p]),
            ("archive_read_support_format_all", c.c_int, [p]),
            ("archive_read_open_filename_w", c.c_int, [p, c.c_wchar_p, c.c_size_t]),
            ("archive_read_open_memory", c.c_int, [p, p, c.c_size_t]),
            ("archive_read_next_header", c.c_int, [p, c.POINTER(p)]),
            ("archive_read_data", c.c_int64, [p, p, c.c_size_t]),
            ("archive_read_free", c.c_int, [p]),
            ("archive_error_string", c.c_char_p, [p]),
            ("archive_entry_pathname_w", c.c_wchar_p, [p]),
            ("archive_entry_pathname", c.c_char_p, [p]),
            ("archive_entry_size", c.c_int64, [p]),
            ("archive_entry_size_is_set", c.c_int, [p]),
            ("archive_entry_mtime", c.c_int64, [p]),
            ("archive_entry_filetype", c.c_ushort, [p]),
            ("archive_entry_is_encrypted", c.c_int, [p]),
        ):
            fn = getattr(dll, name)
            fn.restype, fn.argtypes = res, args
        _dll = dll
    return _dll or None


class _Library(_Container):
    """7z, RAR, CAB, ISO, LZH, …: read front to back, so reading entries in order is cheap."""

    def __init__(self, src):
        super().__init__()
        self.dll = _library()
        if self.dll is None:
            raise NeedsWindows()
        self.src = src
        self._memory = ctypes.create_string_buffer(src, len(src)) if isinstance(src, bytes) else None
        self._entries: list[Entry] | None = None
        self._index: dict[str, int] = {}  # entry name -> its place among all the archive's headers
        self._reader = None  # (archive, place of the last header read)

    def _open(self):
        a = self.dll.archive_read_new()
        self.dll.archive_read_support_filter_all(a)
        self.dll.archive_read_support_format_all(a)
        if self._memory is not None:
            r = self.dll.archive_read_open_memory(a, self._memory, len(self.src))
        else:
            r = self.dll.archive_read_open_filename_w(a, self.src, 1 << 16)
        if r != 0:
            message = self._error(a)
            self.dll.archive_read_free(a)
            raise _problem(message)
        return a

    def _error(self, a) -> str:
        message = self.dll.archive_error_string(a)
        return message.decode("utf-8", "replace") if message else "unreadable archive"

    def entries(self):
        if self._entries is None:
            a = self._open()
            out, entry, place = [], ctypes.c_void_p(), -1
            try:
                while len(out) <= MAX_ENTRIES_BIG:
                    r = self.dll.archive_read_next_header(a, ctypes.byref(entry))
                    if r == _EOF:
                        break
                    if r < _WARN:
                        raise _problem(self._error(a))
                    place += 1
                    if self.dll.archive_entry_filetype(entry) & 0o170000 != _REGULAR:
                        continue
                    name = self.dll.archive_entry_pathname_w(entry)
                    if not name:
                        raw = self.dll.archive_entry_pathname(entry) or b""
                        name = raw.decode("utf-8", "replace")
                    size = self.dll.archive_entry_size(entry) if self.dll.archive_entry_size_is_set(entry) else 0
                    out.append(Entry(name, max(0, size), float(self.dll.archive_entry_mtime(entry)),
                                     encrypted=bool(self.dll.archive_entry_is_encrypted(entry))))
                    self._index.setdefault(name, place)
            finally:
                self.dll.archive_read_free(a)
            self._entries = out
        return self._entries

    def read_entry(self, entry, limit):
        target = self._index[entry.name]
        if self._reader is None or self._reader[1] >= target:
            self._drop_reader()
            self._reader = (self._open(), -1)
        a, place = self._reader
        try:
            header = ctypes.c_void_p()
            while place < target:
                r = self.dll.archive_read_next_header(a, ctypes.byref(header))
                if r == _EOF:
                    raise FileNotFoundError(entry.name)
                if r < _WARN:
                    raise _problem(self._error(a))
                place += 1
            self._reader = (a, place)
            data, buf = bytearray(), ctypes.create_string_buffer(1 << 16)
            while True:
                n = self.dll.archive_read_data(a, buf, len(buf))
                if n == 0:
                    break
                if n < 0:
                    raise _problem(self._error(a))
                data += ctypes.string_at(buf, n)
                if len(data) > limit:
                    break
            return bytes(data)
        except BaseException:
            self._drop_reader()
            raise

    def _drop_reader(self):
        if self._reader is not None:
            self.dll.archive_read_free(self._reader[0])
            self._reader = None

    def close(self):
        self._drop_reader()


def _problem(message: str) -> Exception:
    lower = message.lower()
    if "passphrase" in lower or "encrypt" in lower or "password" in lower:
        return Encrypted(message)
    return ArchiveBroken(message)


def _open(src, label: str) -> _Container:
    kind = container_type(label)
    if kind == "zip":
        return _Zip(src)
    if kind == "tar":
        return _Tar(src)
    if kind == "single":
        return _Single(src, label)
    if kind == "library":
        return _Library(src)
    if kind == "mail":
        return _Mail(src)
    raise ValueError(f"not a container: {label}")


# ---- open containers, reused while indexing --------------------------------------------
# A session keeps containers open between reads (reading a ZIP's directory, or a 7z up to an
# entry, for every file inside would be slow). Outside one, everything is closed right away,
# so the app never holds your files open (Windows wouldn't let you move or delete them).

_lock = threading.RLock()
_open_now: "OrderedDict[tuple, _Container]" = OrderedDict()
_sessions = 0
MAX_OPEN = 4


@contextlib.contextmanager
def session():
    global _sessions
    with _lock:
        _sessions += 1
    try:
        yield
    finally:
        with _lock:
            _sessions -= 1
            if not _sessions:
                release()


def release():
    """Close every container kept open."""
    with _lock:
        while _open_now:
            _open_now.popitem()[1].close()


def _handle(disk: str, names: tuple[str, ...]) -> _Container:
    st = os.stat(disk)
    key = (os.path.normcase(disk), st.st_mtime_ns, st.st_size, tuple(n.lower() for n in names))
    found = _open_now.get(key)
    if found is not None:
        _open_now.move_to_end(key)
        return found
    if names:
        data = _handle(disk, names[:-1]).read(names[-1], MAX_NESTED_BYTES)
        container = _open(data, names[-1])
    else:
        container = _open(disk, disk)
    _open_now[key] = container
    while len(_open_now) > MAX_OPEN:
        _open_now.popitem(last=False)[1].close()
    return container


# ---- paths through containers ------------------------------------------------------------


def _label(disk: str, names) -> str:
    return "\\".join([disk, *(n.replace("/", "\\") for n in names)])


def split(path: str) -> tuple[str, list[str]] | None:
    """(the file on disk, [the names inside it]) for a path through containers: one name, or
    two for a file in a container in a container. None for an ordinary path."""
    parts = path.split("\\")
    for k in range(1, len(parts)):
        if container_type(parts[k - 1]) and os.path.isfile(prefix := "\\".join(parts[:k])):
            rest = parts[k:]
            for j in range(len(rest) - 1):
                if container_type(rest[j]):
                    outer = "/".join(rest[:j + 1])
                    with _lock:
                        try:
                            inner = _handle(prefix, ()).find(outer) is not None
                        except Exception:
                            inner = False
                        finally:
                            if not _sessions:
                                release()
                    if inner:
                        return prefix, [outer, "/".join(rest[j + 1:])]
            return prefix, ["/".join(rest)]
    return None


def exists(path: str) -> bool:
    """True for files on disk and for paths into a container that exists."""
    return os.path.exists(path) or split(path) is not None


def read(path: str, limit: int) -> bytes:
    """One entry's bytes. Refuses more than `limit`, whatever the container claims."""
    where = split(path)
    if where is None:
        raise FileNotFoundError(path)
    disk, names = where
    with session(), _lock:
        return _handle(disk, tuple(names[:-1])).read(names[-1], limit)


def walk(disk: str, mtime: float, look_inside: Callable[[str], bool], nested: bool = True,
         max_entries: int = MAX_ENTRIES) -> Iterator[Item]:
    """Everything inside the container `disk` (a file on disk), and inside the containers in it
    that look_inside(name) allows, if `nested`. What can't be searched comes with its reason."""
    with session():
        yield from _walk(disk, (), mtime, 2 if nested else 1, look_inside, max_entries,
                         "nested_deep" if nested else "nested_off")


def _walk(disk, names, mtime, depth, look_inside, max_entries, too_deep):
    label = _label(disk, names)
    base = label.rsplit("\\", 1)[-1]
    try:
        with _lock:
            entries = _handle(disk, names).entries()
    except Encrypted:
        yield Item(label, base, 0, mtime, "encrypted")
        return
    except NeedsWindows:
        yield Item(label, base, 0, mtime, "needs_windows")
        return
    except TooLarge:
        yield Item(label, base, 0, mtime, "nested_large")
        return
    except Exception:
        yield Item(label, base, 0, mtime, "split_archive" if _SPLIT.search(base) else "damaged_archive")
        return
    if len(entries) > max_entries:
        yield Item(label, base, 0, mtime, "too_many")
        return
    seen = set()
    for e in entries:
        parts = parts_of(e.name)
        if parts is None:
            yield Item(f"{label}\\{e.name}", e.name, e.size, mtime, "odd_name")
            continue
        path = f"{label}\\{'\\'.join(parts)}"
        if path.lower() in seen:
            continue  # the same name twice: the first one counts
        seen.add(path.lower())
        when = e.mtime or mtime
        if e.encrypted:
            yield Item(path, parts[-1], e.size, when, "encrypted")
            continue
        if e.inline:
            yield Item(path, parts[-1], e.size, when, "inline_image")
            continue
        yield Item(path, parts[-1], e.size, when)
        if container_type(parts[-1]) and look_inside(parts[-1]):
            if depth <= 1:
                yield Item(path, parts[-1], e.size, when, too_deep)
            elif e.size > MAX_NESTED_BYTES:
                yield Item(path, parts[-1], e.size, when, "nested_large")
            else:
                yield from _walk(disk, (*names, "/".join(parts)), when, depth - 1, look_inside, max_entries,
                                 "nested_deep")


@contextlib.contextmanager
def extracted(path: str, limit: int) -> Iterator[Path]:
    """The entry as a temporary file with the same name, removed again afterwards."""
    tmp = Path(tempfile.mkdtemp(prefix="quiekel-"))
    try:
        file = tmp / path.rsplit("\\", 1)[-1]
        file.write_bytes(read(path, limit))
        yield file
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def open_copy(path: str, limit: int) -> Path:
    """A copy of the entry to open in its app (which needs a real file), in a temp folder."""
    where = split(path)
    if where is None:
        raise FileNotFoundError(path)
    disk, names = where
    folder = OPENED / f"{Path(disk).stem}-{hashlib.sha1(disk.lower().encode()).hexdigest()[:8]}"
    target = folder.joinpath(*(p for n in names for p in (parts_of(n) or ["file"]))).resolve()
    if not target.is_relative_to(folder.resolve()):  # never outside our folder
        raise ValueError("bad name")
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(read(path, limit))
    return target


def clear_opened():
    """Copies of opened files from earlier sessions are no longer needed."""
    shutil.rmtree(OPENED, ignore_errors=True)
