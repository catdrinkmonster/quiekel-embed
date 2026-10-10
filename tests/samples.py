"""Sample files of the formats the app reads, made from scratch for the tests: OLE compound files
(Office 97-2003, Outlook emails), OpenDocument, e-books, camera raw files."""

import io
import struct
import zipfile
from datetime import datetime, timezone

from PIL import Image

_END, _FREE, _FAT, _NONE = 0xFFFFFFFE, 0xFFFFFFFF, 0xFFFFFFFD, 0xFFFFFFFF


def ole(tree: dict) -> bytes:
    """An OLE compound file: tree is {name: bytes (a stream) or dict (a storage)}. Streams get at
    least 4,096 bytes (zeros after their content), so all of them live in ordinary sectors."""
    entries = [{"name": "Root Entry", "type": 5, "data": b"", "children": []}]

    def add(name, value):
        entries.append({"name": name, "type": 1 if isinstance(value, dict) else 2, "children": [],
                        "data": b"" if isinstance(value, dict) else bytes(value).ljust(4096, b"\x00")})
        i = len(entries) - 1
        if isinstance(value, dict):
            entries[i]["children"] = [add(k, v) for k, v in value.items()]
        return i

    entries[0]["children"] = [add(k, v) for k, v in tree.items()]
    data_sectors = [-(-len(e["data"]) // 512) for e in entries]
    n_dir = -(-len(entries) // 4)
    n_fat = 1
    while n_fat * 128 < n_fat + n_dir + sum(data_sectors):
        n_fat += 1
    fat = [_FAT] * n_fat
    first_dir = len(fat)
    fat += [first_dir + i + 1 for i in range(n_dir - 1)] + [_END]
    starts = []
    for e, n in zip(entries, data_sectors):
        if n:
            starts.append(len(fat))
            fat += [len(fat) + i + 1 for i in range(n - 1)] + [_END]
        else:
            starts.append(_END if e["type"] == 5 else 0)
    fat += [_FREE] * (n_fat * 128 - len(fat))

    links = {i: [_NONE, _NONE, _NONE] for i in range(len(entries))}  # left, right, child
    for i, e in enumerate(entries):
        kids = sorted(e["children"], key=lambda k: (len(entries[k]["name"]), entries[k]["name"].upper()))
        if kids:
            links[i][2] = kids[0]
            for a, b in zip(kids, kids[1:]):
                links[a][1] = b
    directory = bytearray()
    for i, e in enumerate(entries):
        d = bytearray(128)
        name = (e["name"] + "\x00").encode("utf-16-le")
        d[:len(name)] = name
        struct.pack_into("<HBB", d, 64, len(name), e["type"], 1)
        struct.pack_into("<III", d, 68, *links[i])
        struct.pack_into("<IQ", d, 116, starts[i], len(e["data"]))
        directory += d
    while len(directory) % 512:
        unused = bytearray(128)
        struct.pack_into("<III", unused, 68, _NONE, _NONE, _NONE)
        directory += unused

    header = bytearray(512)
    header[:8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"
    struct.pack_into("<HHHHH", header, 0x18, 0x3E, 3, 0xFFFE, 9, 6)
    struct.pack_into("<IIIIIIIII", header, 0x28, 0, n_fat, first_dir, 0, 4096, _END, 0, _END, 0)
    difat = list(range(n_fat)) + [_FREE] * (109 - n_fat)
    struct.pack_into("<109I", header, 0x4C, *difat)
    body = b"".join(struct.pack("<128I", *fat[i * 128:(i + 1) * 128]) for i in range(n_fat))
    body += bytes(directory)
    for e, n in zip(entries, data_sectors):
        body += e["data"].ljust(n * 512, b"\x00")
    return bytes(header) + body


def word_97(text: str) -> bytes:
    """A Word 97-2003 document: `text` as one piece, one byte per character (\\r ends paragraphs)."""
    raw = text.encode("cp1252")
    wd = bytearray(0x800)
    struct.pack_into("<HH", wd, 0, 0xA5EC, 0x00C1)
    struct.pack_into("<H", wd, 0x0A, 0x0200)  # the table stream is "1Table"
    struct.pack_into("<H", wd, 0x20, 14)
    struct.pack_into("<H", wd, 0x3E, 22)
    struct.pack_into("<I", wd, 0x4C, len(text))  # ccpText
    struct.pack_into("<H", wd, 0x98, 93)
    clx = b"\x02" + struct.pack("<I", 16) + struct.pack("<II", 0, len(text)) + struct.pack(
        "<HIH", 0, (0x800 * 2) | 0x40000000, 0)
    struct.pack_into("<II", wd, 0x1A2, 0, len(clx))  # fcClx, lcbClx
    return ole({"WordDocument": bytes(wd) + raw, "1Table": clx})


def _record(rtype, body=b"", ver=0, instance=0):
    return struct.pack("<HHI", ver | instance << 4, rtype, len(body)) + body


def _container(rtype, *children, instance=0):
    return _record(rtype, b"".join(children), ver=0xF, instance=instance)


def powerpoint_97(slides: list[str], master: str = "Click to edit Master title style") -> bytes:
    """A PowerPoint 97-2003 file: each slide's text in the slide list, and a master slide."""
    slide_list = []
    for i, text in enumerate(slides):
        slide_list += [_record(0x03F3, bytes(20)), _record(0x0F9F, bytes(4))]
        slide_list.append(_record(0x0FA0, text.encode("utf-16-le")) if i % 2 == 0
                          else _record(0x0FA8, text.encode("latin-1")))
    document = _container(0x03E8, _container(0x0FF0, *slide_list, instance=0),
                          _container(0x0FF0, _record(0x0FA0, master.encode("utf-16-le")), instance=1))
    stream = document + _container(0x03F8, _record(0x0FA0, master.encode("utf-16-le")))
    return ole({"PowerPoint Document": stream, "Current User": bytes(16)})


def _biff(rtype, body=b""):
    return struct.pack("<HH", rtype, len(body)) + body


def excel_97(sheet: str, rows: list[tuple]) -> bytes:
    """An Excel 97-2003 workbook with one sheet; cells are strings or numbers."""
    strings = sorted({v for row in rows for v in row if isinstance(v, str)})
    bof = lambda kind: _biff(0x0809, struct.pack("<HHHHII", 0x0600, kind, 0x0DBB, 0x07CC, 0, 6))  # noqa: E731
    sst = _biff(0x00FC, struct.pack("<II", len(strings), len(strings)) + b"".join(
        struct.pack("<HB", len(s), 0) + s.encode("latin-1") for s in strings))
    name = sheet.encode("latin-1")
    boundsheet = lambda at: _biff(0x0085, struct.pack("<IBBBB", at, 0, 0, len(name), 0) + name)  # noqa: E731
    head = bof(0x0005) + _biff(0x0042, struct.pack("<H", 1252)) + _biff(0x00E0, bytes(20))
    tail = sst + _biff(0x000A)
    workbook = head + boundsheet(len(head) + len(boundsheet(0)) + len(tail)) + tail
    cells = b""
    for r, row in enumerate(rows):
        for c, value in enumerate(row):
            if isinstance(value, str):
                cells += _biff(0x00FD, struct.pack("<HHHI", r, c, 0, strings.index(value)))
            else:
                cells += _biff(0x0203, struct.pack("<HHHd", r, c, 0, float(value)))
    workbook += bof(0x0010) + _biff(0x0200, struct.pack("<IIHHH", 0, len(rows), 0, 4, 0)) + cells + _biff(0x000A)
    return ole({"Workbook": workbook})


def _filetime(dt: datetime) -> int:
    return int((dt - datetime(1601, 1, 1, tzinfo=timezone.utc)).total_seconds() * 10_000_000)


def outlook_msg(subject: str, body: str, sender=("Stadtwerke", "info@stadtwerke.example"), to="Max Muster",
                sent=datetime(2026, 3, 3, 9, 15, tzinfo=timezone.utc), attachments=()) -> bytes:
    """An Outlook .msg email; attachments: (name, bytes) pairs (give them 4,096 bytes or more)."""
    def text(pid, value):
        return f"__substg1.0_{pid:04X}001F", value.encode("utf-16-le")

    tree = dict([text(0x0037, subject), text(0x0C1A, sender[0]), text(0x5D01, sender[1]), text(0x0E04, to),
                 text(0x1000, body)])
    tree["__properties_version1.0"] = bytes(32) + struct.pack("<IIQ", 0x0039 << 16 | 0x0040, 6, _filetime(sent))
    for i, (name, data) in enumerate(attachments):
        tree[f"__attach_version1.0_#{i:08X}"] = {
            "__substg1.0_3707001F": name.encode("utf-16-le"),
            "__substg1.0_37010102": data,
            "__properties_version1.0": bytes(8) + struct.pack("<IIQ", 0x3705 << 16 | 0x0003, 6, 1),
        }
    return ole(tree)


_ODF_NS = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
           'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
           'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
           'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"')


def opendocument(path, body: str, kind: str = "text", thumbnail: bytes | None = None, encrypted=False):
    """An OpenDocument file (LibreOffice) whose body (office:text, office:spreadsheet, …) is `body`."""
    content = f'<?xml version="1.0" encoding="UTF-8"?><office:document-content {_ODF_NS}>' \
              f'<office:body><office:{kind}>{body}</office:{kind}></office:body></office:document-content>'
    manifest = '<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">' + (
        '<manifest:file-entry manifest:full-path="content.xml"><manifest:encryption-data/></manifest:file-entry>'
        if encrypted else "") + "</manifest:manifest>"
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", f"application/vnd.oasis.opendocument.{kind}")
        z.writestr("content.xml", content)
        z.writestr("META-INF/manifest.xml", manifest)
        if thumbnail:
            z.writestr("Thumbnails/thumbnail.png", thumbnail)
    return path


def epub(path, title: str, text: str):
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml",
                   '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">'
                   '<rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>')
        z.writestr("content.opf",
                   '<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">'
                   f'<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title>'
                   '<dc:identifier id="id">sample</dc:identifier><dc:language>de</dc:language></metadata>'
                   '<manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest>'
                   '<spine><itemref idref="c1"/></spine></package>')
        z.writestr("c1.xhtml", '<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml">'
                               f'<head><title>{title}</title></head><body><p>{text}</p></body></html>')
    return path


def jpeg(size, color) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", size, color).save(buf, "JPEG")
    return buf.getvalue()


def png(size=(200, 150), color=(240, 120, 40)) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", size, color).save(buf, "PNG")
    return buf.getvalue()


def camera_raw(preview_size=(600, 400)) -> bytes:
    """Like a camera's raw file: a TIFF header, raw data stored as lossless JPEG (not a picture),
    a small thumbnail and a big preview."""
    lossless = b"\xff\xd8\xff\xc3\x00\x0b" + bytes(9) + b"\xff\xda\x00\x08" + bytes(6) + bytes(200_000) + b"\xff\xd9"
    return (b"II*\x00\x08\x00\x00\x00" + bytes(1000) + lossless + bytes(500) + jpeg((80, 60), (0, 0, 255))
            + bytes(700) + jpeg(preview_size, (20, 160, 60)) + bytes(300))
