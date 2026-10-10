"""Emails saved as files: .eml (the standard, from Outlook on the web, Thunderbird, Apple Mail,
Windows Mail, …), .msg (Outlook) and saved web pages (.mht, built like an email).

An email's text is its subject, who wrote it to whom and when, and its body. Its attachments
are searched like files inside an archive (see archive.py); pictures shown inside the text
(logos, signatures) are left out."""

import codecs
import email
import email.policy
import mimetypes
import re
import struct
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

from .extract import MAX_CHARS, OLE_MAGIC, Damaged, strip_html

_CID = re.compile(r"""cid:([^"'\s>)]+)""", re.I)


@dataclass
class Attachment:
    name: str
    size: int
    load: Callable[[], bytes]
    inline: bool = False  # a picture shown inside the text


@dataclass
class Mail:
    subject: str = ""
    sender: str = ""
    to: str = ""
    cc: str = ""
    date: str = ""
    body: str = ""
    attachments: list[Attachment] = field(default_factory=list)
    _close: Callable[[], None] | None = None

    def text(self) -> str:
        lines = [f"{label}: {value}" for label, value in (
            ("Subject", self.subject), ("From", self.sender), ("To", self.to), ("Cc", self.cc),
            ("Date", self.date)) if value]
        names = [a.name for a in self.attachments if not a.inline]
        if names:
            lines.append("Attachments: " + ", ".join(names))
        return "\n".join(lines) + "\n\n" + self.body.strip()[:MAX_CHARS]

    def close(self):
        if self._close:
            self._close()
            self._close = None


def read(path: Path) -> Mail:
    """An email's text (its attachments listed, not read)."""
    mail = open_mail(path.read_bytes())
    mail.close()
    return mail


def open_mail(data: bytes) -> Mail:
    """An email with its attachments ready to be read; close() it when done."""
    return _msg(data) if data.startswith(OLE_MAGIC) else _eml(data)


def _when(dt: datetime | None) -> str:
    if dt is None:
        return ""
    try:
        return dt.astimezone().strftime("%Y-%m-%d %H:%M")
    except (ValueError, OverflowError, OSError):
        return ""


def _unique(names: set, name: str) -> str:
    """photo.jpg, photo (2).jpg, …: attachments may share a name."""
    stem, dot, ext = name.rpartition(".")
    if not dot:
        stem, ext = name, ""
    candidate, n = name, 1
    while candidate.lower() in names:
        n += 1
        candidate = f"{stem} ({n}).{ext}" if dot else f"{stem} ({n})"
    names.add(candidate.lower())
    return candidate


# ---- .eml and .mht -------------------------------------------------------------------


def _eml(data: bytes) -> Mail:
    try:
        msg = email.message_from_bytes(data, policy=email.policy.default)
    except Exception:  # the email package is lenient; this is a really broken file
        raise Damaged("email") from None
    mail = Mail(subject=_header(msg, "subject"), sender=_header(msg, "from"), to=_header(msg, "to"),
                cc=_header(msg, "cc"))
    try:
        mail.date = _when(msg["date"].datetime) if msg["date"] is not None else ""
    except (AttributeError, TypeError, ValueError):
        mail.date = _header(msg, "date")
    body = _body(msg, ("plain", "html"))
    if body is not None:
        mail.body = _part_text(body)
    html = _body(msg, ("html",))
    cids = set(_CID.findall(_part_text(html, keep_html=True))) if html is not None else set()
    names: set = set()
    for i, part in enumerate(_leaves(msg)):
        if part is body or part is html:
            continue
        name = _filename(part)
        if part.get_content_type() == "message/rfc822":
            inner = part.get_payload()
            inner = inner[0] if isinstance(inner, list) and inner else None
            payload = inner.as_bytes() if inner is not None else (part.get_payload(decode=True) or b"")
            title = name or (_header(inner, "subject") if inner is not None else "") or "email"
            name = title if title.lower().endswith(".eml") else f"{title}.eml"
        else:
            disposition = part.get_content_disposition()
            if not name and disposition != "attachment":
                continue  # another version of the text (HTML next to plain text), not an attachment
            payload = part.get_payload(decode=True) or b""
            name = name or f"attachment{i + 1}{mimetypes.guess_extension(part.get_content_type()) or '.bin'}"
        cid = (part.get("content-id") or "").strip().strip("<>")
        inline = bool(cid) and cid in cids and part.get_content_disposition() != "attachment"
        mail.attachments.append(Attachment(_unique(names, _clean_name(name)), len(payload),
                                           lambda p=payload: p, inline))
    return mail


def _header(msg, name: str) -> str:
    try:
        value = msg[name]
    except Exception:  # a header so broken that even reading it fails
        return ""
    return " ".join(str(value).split()) if value is not None else ""


def _body(msg, kinds):
    try:
        return msg.get_body(preferencelist=kinds)
    except Exception:
        return None


def _filename(part) -> str:
    try:
        return part.get_filename() or ""
    except Exception:
        return ""


def _leaves(part, top=True):
    """The parts that hold content, in order; an attached email counts as one part."""
    if not top and part.get_content_type() == "message/rfc822":
        yield part
    elif part.is_multipart():
        for sub in part.iter_parts():
            yield from _leaves(sub, top=False)
    else:
        yield part


def _part_text(part, keep_html=False) -> str:
    try:
        text = part.get_content()
    except Exception:  # an unknown character set, or a broken encoding
        payload = part.get_payload(decode=True) or b""
        text = payload.decode("utf-8", "replace")
    if not isinstance(text, str):
        return ""
    return strip_html(text) if part.get_content_type() == "text/html" and not keep_html else text


def _clean_name(name: str) -> str:
    name = " ".join(name.replace("\x00", "").split())
    return name.replace("/", "_").replace("\\", "_").strip(". ") or "attachment"


# ---- .msg (Outlook) ------------------------------------------------------------------
# An OLE compound file ([MS-OXMSG]): properties as streams named after their ID and type.

_UNICODE, _STRING8, _BINARY = 0x001F, 0x001E, 0x0102


class _Props:
    """The properties of the message, an attachment, or an attached message."""

    def __init__(self, ole, prefix: str, header: int):
        self.ole, self.prefix, self.fixed = ole, prefix, {}
        name = prefix + "__properties_version1.0"
        if ole.exists(name):
            data = ole.openstream(name).read()
            for at in range(header, len(data) - 15, 16):
                tag = struct.unpack_from("<I", data, at)[0]
                self.fixed[tag >> 16] = (tag & 0xFFFF, data[at + 8:at + 16])
        self.codepage = _codec(self.int(0x3FDE) or self.int(0x3FFD))

    def int(self, pid: int) -> int | None:
        value = self.fixed.get(pid)
        if value and value[0] in (0x0003, 0x000B):
            return struct.unpack_from("<i", value[1])[0]
        return None

    def time(self, pid: int) -> datetime | None:
        value = self.fixed.get(pid)
        if value and value[0] == 0x0040:
            filetime = struct.unpack_from("<Q", value[1])[0]
            if filetime:
                try:
                    return datetime(1601, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=filetime // 10)
                except OverflowError:
                    return None
        return None

    def raw(self, pid: int, ptype: int) -> bytes | None:
        name = f"{self.prefix}__substg1.0_{pid:04X}{ptype:04X}"
        return self.ole.openstream(name).read() if self.ole.exists(name) else None

    def str(self, pid: int) -> str:
        data = self.raw(pid, _UNICODE)
        if data is not None:
            return data.decode("utf-16-le", "replace").rstrip("\x00")
        data = self.raw(pid, _STRING8)
        return data.decode(self.codepage, "replace").rstrip("\x00") if data is not None else ""

    def html(self) -> str:
        data = self.raw(0x1013, _BINARY)
        if data is not None:
            return data.decode(self.codepage, "replace")
        return self.str(0x1013)

    def body(self) -> str:
        text = self.str(0x1000)
        if text.strip():
            return text
        html = self.html()
        if html.strip():
            return strip_html(html)
        rtf = self.raw(0x1009, _BINARY)  # compressed RTF, the oldest kind of body
        if rtf:
            from .documents import rtf_text

            try:
                return rtf_text(decompress_rtf(rtf).decode("latin-1"))
            except ValueError:
                pass
        return ""


def _codec(codepage: int | None) -> str:
    if codepage in (None, 0):
        return "cp1252"
    name = {65001: "utf-8", 20127: "ascii", 28591: "latin-1", 1200: "utf-16-le"}.get(codepage, f"cp{codepage}")
    try:
        codecs.lookup(name)
        return name
    except LookupError:
        return "cp1252"


def _msg(data: bytes) -> Mail:
    import olefile

    try:
        ole = olefile.OleFileIO(data)
    except OSError:
        raise Damaged("email") from None
    try:
        props = _Props(ole, "", 32)
        mail = Mail(subject=props.str(0x0037), to=props.str(0x0E04), cc=props.str(0x0E03),
                    date=_when(props.time(0x0039) or props.time(0x0E06)), body=props.body())
        name, address = props.str(0x0C1A) or props.str(0x0042), props.str(0x5D01) or props.str(0x0C1F)
        if "@" not in address:
            address = ""  # an Exchange address (/O=…) means nothing outside the company
        mail.sender = f"{name} <{address}>" if name and address and name != address else (name or address)
        cids = set(_CID.findall(props.html()))
        storages = sorted({e[0] for e in ole.listdir(streams=True, storages=True)
                           if e and e[0].lower().startswith("__attach_version1.0_#")})
        names: set = set()
        for i, storage in enumerate(storages):
            att = _Props(ole, storage + "/", 8)
            method = att.int(0x3705) or 1
            if method == 5:  # an attached email: its text becomes part of this one's
                inner = _Props(ole, storage + "/__substg1.0_3701000D/", 24)
                mail.body += f"\n\n--- {inner.str(0x0037)} ---\n{inner.body()}"
                continue
            stream = storage + "/__substg1.0_37010102"
            if not ole.exists(stream):
                continue  # a link to a file elsewhere, or an embedded object
            label = att.str(0x3707) or att.str(0x3704) or att.str(0x3001) or f"attachment{i + 1}"
            cid = att.str(0x3712).strip("<>")
            inline = bool(att.int(0x7FFE)) or bool((att.int(0x3714) or 0) & 0x4) or (bool(cid) and cid in cids)
            mail.attachments.append(Attachment(_unique(names, _clean_name(label)), ole.get_size(stream),
                                               lambda s=stream: ole.openstream(s).read(), inline))
    except Damaged:
        ole.close()
        raise
    except Exception:
        ole.close()
        raise Damaged("email") from None
    mail._close = ole.close
    return mail


# Compressed RTF ([MS-OXRTFCP]): LZ77 with a dictionary that starts out with common RTF.
_RTF_DICTIONARY = (
    b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern "
    b"\\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0"
    b"\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx"
)


def decompress_rtf(data: bytes) -> bytes:
    if len(data) < 16:
        raise ValueError("too short")
    size, raw_size, magic = struct.unpack_from("<III", data, 0)
    if magic == 0x414C454D:  # "MELA": stored as is
        return data[16:16 + raw_size]
    if magic != 0x75465A4C:  # "LZFu"
        raise ValueError("not compressed RTF")
    window = bytearray(4096)
    window[:len(_RTF_DICTIONARY)] = _RTF_DICTIONARY
    write = len(_RTF_DICTIONARY)
    out = bytearray()
    pos, end = 16, min(len(data), size + 4)
    while pos < end and len(out) < raw_size:
        control = data[pos]
        pos += 1
        for bit in range(8):
            if pos >= end:
                break
            if control & (1 << bit):
                if pos + 1 >= end:
                    return bytes(out)
                ref = data[pos] << 8 | data[pos + 1]
                pos += 2
                offset, length = ref >> 4, (ref & 0xF) + 2
                if offset == write:  # the end marker
                    return bytes(out)
                for i in range(length):
                    b = window[(offset + i) % 4096]
                    out.append(b)
                    window[write] = b
                    write = (write + 1) % 4096
            else:
                b = data[pos]
                pos += 1
                out.append(b)
                window[write] = b
                write = (write + 1) % 4096
    return bytes(out)
