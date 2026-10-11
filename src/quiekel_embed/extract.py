"""Turn files into embeddable chunks: text passages or images."""

import html
import io
import json
import re
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageFile, ImageOps, UnidentifiedImageError

from . import config

try:
    from pillow_heif import register_heif_opener

    register_heif_opener()
except ImportError:  # HEIC photos are skipped without it
    pass

Image.MAX_IMAGE_PIXELS = 250_000_000
# A photo missing its last few bytes (an interrupted copy) still shows nearly everything.
ImageFile.LOAD_TRUNCATED_IMAGES = True
_QUOTED_PATH = re.compile(r"""['"][A-Za-z]:[\\/][^'"]*['"]""")

MAX_CHARS = config.CHUNK_CHARS * config.MAX_CHUNKS_PER_FILE  # more text than this is never embedded
OLE_MAGIC = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"  # Office 97-2003 files, Outlook emails, …
# Read page by page with PyMuPDF; pages without text (scans, comics) are looked at instead.
PAGED_EXTS = {".pdf", ".epub", ".mobi", ".fb2", ".xps", ".oxps", ".cbz", ".ai"}
# Pictures a file without an extension may hold, by their first bytes (also WebP, HEIC: see sniff).
_PICTURE_STARTS = (b"\xff\xd8\xff", b"\x89PNG\r\n\x1a\n", b"GIF87a", b"GIF89a", b"II*\x00", b"MM\x00*")
HTML_EXTS = {".html", ".htm", ".xhtml"}
# Documents that carry a preview picture of themselves.
THUMBNAIL_EXTS = {".odt", ".ott", ".ods", ".ots", ".odp", ".otp", ".odg", ".otg"}


class SkipFile(Exception):
    """The file can't be searched by its content, and that's not an error: it's empty, too big,
    behind a password, … str(e) is the reason's key; the app puts it in words."""


class Damaged(Exception):
    """The file isn't what its name says, or it's broken. str(e) is what it should have been:
    "document", "email", "image", "archive"."""


@dataclass
class Chunk:
    snippet: str  # shown in search results
    text: str | None = None  # embedded as a document passage
    image: Image.Image | None = None  # embedded as an image


@dataclass
class Extracted:
    kind: str
    chunks: list[Chunk] = field(default_factory=list)


def kind_of(path: Path) -> str | None:
    ext = path.suffix.lower()
    if ext in config.IMAGE_EXTS:
        return "image"
    if ext in config.DOC_EXTS:
        return "doc"
    if ext in config.CODE_EXTS:
        return "code"
    if ext in config.TEXT_EXTS:
        return "text"
    if not ext:
        return config.KNOWN_NAMES.get(path.name.lower())
    return None


def sniff(path: str) -> str | None:
    """What a file without an extension holds, by its first bytes: "image", "text", or None."""
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
    except OSError:
        return None
    if (head.startswith(_PICTURE_STARTS) or (head[:4] == b"RIFF" and head[8:12] == b"WEBP")
            or (head[4:8] == b"ftyp" and head[8:12] in (b"heic", b"heix", b"mif1", b"avif"))):
        return "image"
    if head.startswith((b"\xef\xbb\xbf", b"\xff\xfe", b"\xfe\xff")):  # text that says how it's written
        return "text"
    if not head or b"\x00" in head:
        return None
    try:
        head.decode("utf-8")
    except UnicodeDecodeError as e:
        if e.start < len(head) - 3:  # (only the last character may be cut off)
            return None
    return "text"


def why_not(name: str) -> str:
    """Why a file of a type that isn't searched isn't: "media", "program", "mailbox" or "type"."""
    ext = Path(name).suffix.lower()
    if ext in config.MEDIA_EXTS:
        return "media"
    if ext in config.PROGRAM_EXTS:
        return "program"
    if ext in config.MAILBOX_EXTS:
        return "mailbox"
    return "type"


def problem(e: Exception) -> str:
    """Why a file couldn't be read: "!key" where it's a known case (the app puts it in words),
    else the error itself, without file paths."""
    import tarfile
    import zipfile

    name = type(e).__name__
    if isinstance(e, Damaged):
        return f"!damaged_{e}"
    if isinstance(e, OSError) and getattr(e, "winerror", None) in (32, 33):
        return "!in_use"  # another program has it open, and locked
    if isinstance(e, PermissionError):
        return "!no_access"
    if isinstance(e, FileNotFoundError):
        return "!gone"
    if isinstance(e, MemoryError):
        return "!no_memory"
    if isinstance(e, UnidentifiedImageError) or "image file is truncated" in str(e):
        return "!damaged_image"
    if name in ("FileDataError", "EmptyFileError"):  # PyMuPDF
        return "!damaged_pdf"
    if isinstance(e, (zipfile.BadZipFile, KeyError)) or name in ("PackageNotFoundError", "ParseError",
                                                                    "InvalidFileException", "XLRDError"):
        return "!damaged_document"  # (Office files are ZIPs inside)
    if isinstance(e, tarfile.TarError) or name == "ArchiveBroken":
        return "!damaged_archive"
    return f"{name}: {_QUOTED_PATH.sub('', str(e))}".strip()[:300]


def max_bytes(kind: str | None, name: str = "") -> int:
    """The biggest file of this kind that gets indexed."""
    if kind == "image" and Path(name).suffix.lower() in config.RAW_EXTS | config.LAYERED_EXTS:
        return config.MAX_RAW_BYTES  # only the picture inside is read
    return {"image": config.MAX_IMAGE_BYTES, "doc": config.MAX_DOC_BYTES}.get(kind, config.MAX_TEXT_BYTES)


def has_preview(name: str, kind: str) -> bool:
    """Whether a file can be shown as a picture: images, and documents with pages to show."""
    ext = Path(name).suffix.lower()
    return kind == "image" or ext in PAGED_EXTS or ext in THUMBNAIL_EXTS


def extract(path: Path, size: int, title: str | None = None, kind: str | None = None) -> Extracted:
    """title: what the model sees as the document title (defaults to the file name).
    kind: how to read it, when not by its name (types added in Settings are read as text)."""
    kind = kind or kind_of(path) or "text"
    ext = path.suffix.lower()
    if size == 0:
        raise SkipFile("empty")
    if size > max_bytes(kind, path.name):
        raise SkipFile("too_large")
    if kind == "image":
        try:
            image = load_image(path)
        except UnidentifiedImageError:
            if _head(path, 4) == b"PK\x03\x04":  # an archive with a picture's name, e.g. an animated sticker
                raise SkipFile("not_image") from None
            raise
        return Extracted(kind, [Chunk(snippet="", image=image)])

    if kind == "doc":
        text, pages = read_document(path)
        if not text.strip() and pages:
            return Extracted(kind, [
                Chunk(snippet=f"Page {i + 1} (scanned)", image=img)
                for i, img in enumerate(pages)
            ])
    else:
        text = read_text(path)
        if ext == ".ipynb":
            text = notebook_text(text)
        elif ext in HTML_EXTS:
            text = strip_html(text)

    title = title or path.name
    chunks = [
        Chunk(snippet=c, text=f"title: {title} | text: {c}") for c in chunk_text(text)
    ]
    if not chunks:
        raise SkipFile("no_text")
    return Extracted(kind, chunks)


# ---- readers -------------------------------------------------------------


def _head(path: Path, n: int) -> bytes:
    with path.open("rb") as f:
        return f.read(n)


def read_document(path: Path) -> tuple[str, list[Image.Image]]:
    """A document's text, or for one without any (a scan), its first pages as pictures."""
    ext = path.suffix.lower()
    if ext in PAGED_EXTS:
        return read_paged(path)
    if ext in (".eml", ".msg", ".mht", ".mhtml"):
        from . import mail

        if ext == ".msg" and _head(path, 8) != OLE_MAGIC:
            return plain_text(path), []  # not an Outlook email: other programs use .msg too
        m = mail.read(path)
        if ext in (".mht", ".mhtml"):
            m.attachments = []  # a saved web page's pictures and styles
        return m.text(), []
    from . import documents

    return documents.read(path), []


def read_text(path: Path) -> str:
    return decode_text(path.read_bytes())


def decode_text(data: bytes) -> str:
    if data.startswith((b"\xff\xfe\x00\x00", b"\x00\x00\xfe\xff")):
        return data.decode("utf-32", errors="replace")
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):  # UTF-16, like Windows' .reg and .inf files
        return data.decode("utf-16", errors="replace")
    if b"\x00" in data[:8192]:
        raise SkipFile("binary")
    try:
        return data.decode("utf-8-sig")
    except UnicodeDecodeError:
        return data.decode("cp1252", errors="replace")


def plain_text(path: Path) -> str:
    """A file read as text, without tags if it's a web page."""
    with path.open("rb") as f:
        text = decode_text(f.read(config.MAX_TEXT_BYTES))
    return strip_html(text) if text.lstrip()[:1] == "<" else text


def notebook_text(raw: str) -> str:
    try:
        nb = json.loads(raw)
    except json.JSONDecodeError:
        return raw
    parts = []
    for cell in nb.get("cells", []):
        src = cell.get("source", "")
        parts.append("".join(src) if isinstance(src, list) else str(src))
    return "\n\n".join(parts)


_SCRIPT_RE = re.compile(r"<(script|style)\b.*?</\1>", re.S | re.I)
_TAG_RE = re.compile(r"<[^>]+>")


def strip_html(raw: str) -> str:
    return html.unescape(_TAG_RE.sub(" ", _SCRIPT_RE.sub(" ", raw)))


def read_paged(path: Path) -> tuple[str, list[Image.Image]]:
    """PDF, e-books (EPUB, MOBI, FB2), XPS and comic archives, through PyMuPDF."""
    import pymupdf

    with pymupdf.open(path, filetype=_paged_type(path)) as doc:
        if doc.needs_pass:
            raise SkipFile("password")
        parts = []
        for page in doc:
            parts.append(page.get_text())
            if sum(len(p) for p in parts) > MAX_CHARS:
                break
        text = "\n\n".join(parts)
        pages = []
        if not text.strip():
            for page in doc.pages(0, min(config.SCANNED_PDF_PAGES, doc.page_count)):
                pix = page.get_pixmap(dpi=100)
                img = Image.frombytes("RGB", (pix.width, pix.height), pix.samples)
                img.thumbnail((config.MAX_IMAGE_SIDE, config.MAX_IMAGE_SIDE))
                pages.append(img)
    return text, pages


# Kept for code that reads PDFs directly.
read_pdf = read_paged


def _paged_type(path: Path) -> str | None:
    """How PyMuPDF should open a file: by its name, except Illustrator's, which are PDFs inside."""
    return "pdf" if path.suffix.lower() == ".ai" else None


def layered_picture(path: Path) -> bytes:
    """The finished picture a layered painting carries (Krita, OpenRaster): its layers merged."""
    import zipfile

    try:
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())
            for name in ("mergedimage.png", "preview.png", "Thumbnails/thumbnail.png"):
                if name in names:
                    return z.read(name)
    except zipfile.BadZipFile:
        pass
    raise Damaged("image")


def load_image(path: Path, max_side: int = config.MAX_IMAGE_SIDE) -> Image.Image:
    ext = path.suffix.lower()
    if ext == ".svg":
        return _svg(path, max_side)
    if ext in config.RAW_EXTS:
        source = io.BytesIO(raw_preview(path))
    elif ext in config.LAYERED_EXTS:
        source = io.BytesIO(layered_picture(path))
    else:
        source = path
    with Image.open(source) as src:
        if min(src.size) < config.MIN_IMAGE_SIDE:
            raise SkipFile("tiny_image")
        # JPEG can decode at reduced size directly, which is much faster for big photos.
        src.draft("RGB", (max_side * 2, max_side * 2))
        img = ImageOps.exif_transpose(src)
        if img.mode in ("RGBA", "LA", "P", "PA"):
            img = img.convert("RGBA")
            bg = Image.new("RGB", img.size, (255, 255, 255))
            bg.paste(img, mask=img.getchannel("A"))
            img = bg
        else:
            img = img.convert("RGB")
    img.thumbnail((max_side, max_side))
    return img


def _svg(path: Path, max_side: int) -> Image.Image:
    import pymupdf

    with pymupdf.open(path) as doc:  # PyMuPDF draws SVG
        page = doc[0]
        w, h = page.rect.width, page.rect.height
        if min(w, h) < config.MIN_IMAGE_SIDE:
            raise SkipFile("tiny_image")  # icons
        zoom = min(max_side / max(w, h), 4.0)
        pix = page.get_pixmap(matrix=pymupdf.Matrix(zoom, zoom), alpha=False)
    return Image.frombytes("RGB", (pix.width, pix.height), pix.samples)


def raw_preview(path: Path) -> bytes:
    """A camera raw file's biggest preview picture: every camera stores a JPEG inside."""
    import mmap

    with path.open("rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        found = []
        at = m.find(b"\xff\xd8\xff")
        while at != -1:
            end = _jpeg_end(m, at)
            if end:
                found.append((end - at, at, end))
            at = m.find(b"\xff\xd8\xff", end or at + 3)
        for _, start, end in sorted(found, reverse=True):
            data = m[start:end]
            try:
                with Image.open(io.BytesIO(data)) as img:
                    if min(img.size) >= config.MIN_IMAGE_SIDE:
                        return data
            except (OSError, ValueError):
                continue
    raise Damaged("image")


def _jpeg_end(m, start: int) -> int:
    """Where the JPEG at `start` ends (0: it isn't one, or it's the camera's raw data stored as
    lossless JPEG, which isn't a picture to look at)."""
    pos, n = start + 2, len(m)
    while pos + 4 <= n:
        if m[pos] != 0xFF:
            return 0
        marker = m[pos + 1]
        if marker == 0xFF:  # fill byte
            pos += 1
            continue
        if marker == 0xD8 or marker == 0x01 or 0xD0 <= marker <= 0xD7:
            pos += 2
            continue
        if marker in (0xC3, 0xC7, 0xCB, 0xCF):  # lossless
            return 0
        length = m[pos + 2] << 8 | m[pos + 3]
        if length < 2:
            return 0
        pos += 2 + length
        if marker != 0xDA:  # not yet the image data
            continue
        while True:  # the image data runs to the next marker
            pos = m.find(b"\xff", pos)
            if pos == -1 or pos + 1 >= n:
                return 0
            following = m[pos + 1]
            if following == 0x00 or 0xD0 <= following <= 0xD7:
                pos += 2
            elif following == 0xFF:
                pos += 1
            elif following == 0xD9:
                return pos + 2
            else:
                break  # another segment (progressive JPEGs have several scans)
    return 0


def picture(path: Path, size: int) -> Image.Image:
    """A file as a picture, for previews: an image itself, a document's first page, or the
    preview picture a document carries."""
    ext = path.suffix.lower()
    if ext in PAGED_EXTS or ext == ".svg":
        import pymupdf

        with pymupdf.open(path, filetype=_paged_type(path)) as doc:
            if doc.needs_pass or not doc.page_count:
                raise ValueError("nothing to show")
            page = doc[0]
            zoom = size / max(page.rect.width, page.rect.height, 1)
            pix = page.get_pixmap(matrix=pymupdf.Matrix(zoom, zoom), alpha=False)
        return Image.frombytes("RGB", (pix.width, pix.height), pix.samples)
    if ext in THUMBNAIL_EXTS:
        from . import documents

        data = documents.odf_thumbnail(path)
        if not data:
            raise ValueError("no preview picture inside")
        with Image.open(io.BytesIO(data)) as img:
            return img.convert("RGB")
    return load_image(path, size)


# ---- chunking ------------------------------------------------------------

_BLANK_LINES_RE = re.compile(r"\n{3,}")
_TRAILING_RE = re.compile(r"[^\S\n]+\n")
# Collapse runs of spaces inside a line, but keep leading indentation (code).
_INNER_SPACES_RE = re.compile(r"(?<=\S)[^\S\n]{2,}")


def chunk_text(text: str) -> list[str]:
    text = text.replace("\r\n", "\n").replace("\r", "\n").replace("\t", "    ")
    text = _TRAILING_RE.sub("\n", text)
    text = _INNER_SPACES_RE.sub(" ", text)
    text = _BLANK_LINES_RE.sub("\n\n", text).strip()
    if not text:
        return []
    size, overlap = config.CHUNK_CHARS, config.CHUNK_OVERLAP
    chunks, start, n = [], 0, len(text)
    while start < n and len(chunks) < config.MAX_CHUNKS_PER_FILE:
        end = min(start + size, n)
        if end < n:
            # Break at the strongest boundary in the back half of the window.
            window = text[start:end]
            for sep in ("\n\n", "\n", ". ", " "):
                cut = window.rfind(sep)
                if cut > size // 2:
                    end = start + cut + len(sep)
                    break
        chunk = text[start:end].strip()
        if chunk:
            chunks.append(chunk)
        if end >= n:
            break
        start = end - overlap
        space = text.find(" ", start, end)
        if space != -1:
            start = space + 1
    return chunks
