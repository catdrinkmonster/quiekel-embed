"""Office documents as plain text: Microsoft Office (also its macro, template and slide-show
variants), Office 97-2003, OpenDocument (LibreOffice, OpenOffice) and RTF."""

import io
import re
import struct
import zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

from . import config
from .extract import MAX_CHARS, OLE_MAGIC, Damaged, SkipFile

MAX_ROWS = 5000  # spreadsheets: rows read per file
MAX_PART_BYTES = 256 * 1024 * 1024  # one part of a document, unpacked (a ZIP can lie about sizes)

ODF_EXTS = {".odt", ".ott", ".fodt", ".ods", ".ots", ".fods", ".odp", ".otp", ".fodp", ".odg", ".otg", ".fodg"}
WORD_EXTS = {".docx", ".docm", ".dotx", ".dotm"}
EXCEL_EXTS = {".xlsx", ".xlsm", ".xltx", ".xltm"}
POWERPOINT_EXTS = {".pptx", ".pptm", ".ppsx", ".ppsm", ".potx", ".potm"}
OLD_OFFICE_EXTS = {".doc", ".dot", ".xls", ".xlt", ".ppt", ".pps", ".pot"}


def read(path: Path) -> str:
    """The text of an office document or RTF file, whatever its exact flavour."""
    ext = path.suffix.lower()
    with path.open("rb") as f:
        head = f.read(8)
    if head.startswith(OLE_MAGIC):  # Office 97-2003, or a password-protected modern Office file
        return read_ole(path)
    if head.startswith(b"{\\rtf"):  # (Word also saves RTF under .doc)
        return read_rtf(path)
    if head.startswith(b"PK"):
        if ext in ODF_EXTS:
            return read_odf(path)
        if ext in EXCEL_EXTS:
            return read_xlsx(path)
        if ext in POWERPOINT_EXTS:
            return read_pptx(path)
        return read_ooxml(path)  # a .docx, or a modern file under an old name
    if ext in {".fodt", ".fods", ".fodp", ".fodg"}:
        return read_odf(path)
    # A web page or plain text saved under an Office name (old Excel "web page" files, …)
    from .extract import decode_text, strip_html

    text = decode_text(path.read_bytes()[:config.MAX_TEXT_BYTES])
    return strip_html(text) if text.lstrip()[:1] == "<" else text


# ---- Microsoft Office -----------------------------------------------------------------

_WORD_TYPES = (
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
)


def read_ooxml(path: Path) -> str:
    """A Word, Excel or PowerPoint file (2007 and later), told apart by what's inside."""
    with zipfile.ZipFile(path) as z:
        names = set(z.namelist())
    if "word/document.xml" in names:
        return read_docx(path)
    if "xl/workbook.xml" in names:
        return read_xlsx(path)
    if "ppt/presentation.xml" in names:
        return read_pptx(path)
    raise Damaged("document")


def read_docx(path: Path) -> str:
    from docx.opc.part import PartFactory
    from docx.package import Package
    from docx.parts.document import DocumentPart

    # python-docx only opens plain documents; macro documents and templates are built the same.
    for content_type in _WORD_TYPES:
        PartFactory.part_type_for.setdefault(content_type, DocumentPart)
    part = Package.open(str(path)).main_document_part
    if not isinstance(part, DocumentPart):
        raise Damaged("document")
    d = part.document
    parts = [p.text for p in d.paragraphs]
    for table in d.tables:
        for row in table.rows:
            parts.append(" | ".join(c.text.strip() for c in row.cells))
    return "\n".join(parts)


def read_pptx(path: Path) -> str:
    from pptx.package import Package

    # (Presentation() refuses templates and slide shows, which are built the same.)
    presentation = Package.open(str(path)).main_document_part.presentation
    parts = []
    for n, slide in enumerate(presentation.slides, 1):
        parts.append(f"\n\nSlide {n}:")
        for shape in slide.shapes:
            if shape.has_text_frame:
                parts.append(shape.text_frame.text)
            elif getattr(shape, "has_table", False) and shape.has_table:
                for row in shape.table.rows:
                    parts.append(" | ".join(c.text for c in row.cells))
        if slide.has_notes_slide and slide.notes_slide.notes_text_frame is not None:
            notes = slide.notes_slide.notes_text_frame.text.strip()
            if notes:
                parts.append(f"Notes: {notes}")
    return "\n".join(parts)


def read_xlsx(path: Path) -> str:
    import openpyxl

    wb = openpyxl.load_workbook(str(path), read_only=True, data_only=True)
    parts, rows = [], 0
    try:
        for ws in wb.worksheets:
            parts.append(f"\n\nSheet {ws.title}:")
            for row in ws.iter_rows(values_only=True):
                cells = [str(v) for v in row if v is not None]
                if cells:
                    parts.append(" | ".join(cells))
                    rows += 1
                if rows > MAX_ROWS:
                    return "\n".join(parts)
    finally:
        wb.close()
    return "\n".join(parts)


def ooxml_thumbnail(path: Path) -> bytes | None:
    """The preview picture Office saves into some files ("Save thumbnail")."""
    return _zip_picture(path, ("docProps/thumbnail.jpeg", "docProps/thumbnail.jpg", "docProps/thumbnail.png"))


# ---- Office 97-2003 (and password-protected modern files): OLE compound files ---------


def read_ole(path: Path) -> str:
    import olefile

    try:
        ole = olefile.OleFileIO(str(path))
    except OSError:
        raise Damaged("document") from None
    with ole:
        if ole.exists("EncryptedPackage") or ole.exists("EncryptionInfo"):
            raise SkipFile("password")  # a modern Office file with a password
        if ole.exists("WordDocument"):
            return _doc_text(ole)
        if ole.exists("PowerPoint Document"):
            return _ppt_text(ole)
        if ole.exists("Workbook") or ole.exists("Book"):
            return _xls_text(ole)
    raise Damaged("document")


def _u16(b, at):
    return struct.unpack_from("<H", b, at)[0]


def _u32(b, at):
    return struct.unpack_from("<I", b, at)[0]


def _doc_text(ole) -> str:
    """Word 97-2003 ([MS-DOC]): the main text, put together from the pieces the piece table
    lists (fast-saved files keep their text out of order)."""
    wd = ole.openstream("WordDocument").read()
    if len(wd) < 0x40 or _u16(wd, 0) != 0xA5EC:
        raise Damaged("document")
    nfib, flags = _u16(wd, 2), _u16(wd, 0x0A)
    if flags & 0x0100 or flags & 0x8000:  # fEncrypted, fObfuscated
        raise SkipFile("password")
    if nfib < 101:  # Word 6 and 95: one run of single-byte text
        fc_min, fc_mac = _u32(wd, 0x18), _u32(wd, 0x1C)
        return _doc_clean(wd[fc_min:min(fc_mac, fc_min + MAX_CHARS)].decode("cp1252", "replace"))
    table_name = "1Table" if flags & 0x0200 else "0Table"  # fWhichTblStm
    if not ole.exists(table_name):
        raise Damaged("document")
    table = ole.openstream(table_name).read()
    try:
        csw = _u16(wd, 0x20)
        lw = 0x22 + csw * 2 + 2  # FibRgLw97
        ccp_text = _u32(wd, lw + 12)
        rg = lw + _u16(wd, lw - 2) * 4 + 2  # FibRgFcLcb
        if _u16(wd, rg - 2) <= 33:
            raise Damaged("document")
        fc_clx, lcb_clx = struct.unpack_from("<II", wd, rg + 33 * 8)  # fcClx, lcbClx
        text = _pieces(wd, table[fc_clx:fc_clx + lcb_clx], ccp_text)
    except struct.error:
        raise Damaged("document") from None
    return _doc_clean(text)


def _pieces(wd: bytes, clx: bytes, limit: int) -> str:
    i = 0
    while i < len(clx) and clx[i] == 0x01:  # Prc: formatting, not text
        i += 3 + _u16(clx, i + 1)
    if i + 5 > len(clx) or clx[i] != 0x02:
        raise Damaged("document")
    lcb = _u32(clx, i + 1)
    plc = clx[i + 5:i + 5 + lcb]
    n = (lcb - 4) // 12
    if n <= 0:
        return ""
    cps = struct.unpack_from(f"<{n + 1}I", plc, 0)
    out, total = [], 0
    for k in range(n):
        start, end = cps[k], min(cps[k + 1], limit)
        if start >= limit or total > MAX_CHARS:
            break
        if end <= start:
            continue
        fc = _u32(plc, (n + 1) * 4 + k * 8 + 2)
        if fc & 0x40000000:  # fCompressed: one byte per character
            at = (fc & 0x3FFFFFFF) // 2
            out.append(wd[at:at + end - start].decode("cp1252", "replace"))
        else:
            out.append(wd[fc:fc + 2 * (end - start)].decode("utf-16-le", "replace"))
        total += end - start
    return "".join(out)


_DOC_CHARS = str.maketrans({
    "\r": "\n", "\x0b": "\n", "\x0c": "\n", "\x07": " | ", "\x1e": "-", "\xa0": " ",
    **{chr(c): "" for c in (0x01, 0x02, 0x03, 0x04, 0x05, 0x08, 0x13, 0x14, 0x15, 0x1f)},
})


def _doc_clean(text: str) -> str:
    """Word's special characters: paragraph and cell marks into line breaks and bars, objects
    out, and of each field only its result (not its instruction, like HYPERLINK "…")."""
    if "\x13" in text:
        out, fields = [], []  # per open field: still in its instruction?
        for ch in text:
            if ch == "\x13":
                fields.append(True)
            elif ch == "\x14":
                if fields:
                    fields[-1] = False
            elif ch == "\x15":
                if fields:
                    fields.pop()
            elif True not in fields:
                out.append(ch)
        text = "".join(out)
    return text.translate(_DOC_CHARS)


def _xls_text(ole) -> str:
    import xlrd

    data = ole.openstream("Workbook" if ole.exists("Workbook") else "Book").read()
    try:
        book = xlrd.open_workbook(file_contents=data, on_demand=True, logfile=io.StringIO())
    except Exception as e:  # xlrd's own errors, or worse for a broken file
        if "encrypt" in str(e).lower():
            raise SkipFile("password") from None
        raise Damaged("document") from None
    parts, rows = [], 0
    try:
        for i in range(book.nsheets):
            sheet = book.sheet_by_index(i)
            parts.append(f"\n\nSheet {sheet.name}:")
            for r in range(sheet.nrows):
                cells = [_xls_value(c, book.datemode) for c in sheet.row(r)]
                cells = [c for c in cells if c]
                if cells:
                    parts.append(" | ".join(cells))
                    rows += 1
                if rows > MAX_ROWS:
                    return "\n".join(parts)
            book.unload_sheet(i)
    finally:
        book.release_resources()
    return "\n".join(parts)


def _xls_value(cell, datemode) -> str:
    import xlrd

    if cell.ctype in (xlrd.XL_CELL_EMPTY, xlrd.XL_CELL_BLANK, xlrd.XL_CELL_ERROR):
        return ""
    if cell.ctype == xlrd.XL_CELL_DATE:
        try:
            return xlrd.xldate_as_datetime(cell.value, datemode).isoformat(sep=" ").removesuffix(" 00:00:00")
        except (ValueError, OverflowError):
            return str(cell.value)
    if cell.ctype == xlrd.XL_CELL_NUMBER and float(cell.value).is_integer():
        return str(int(cell.value))
    if cell.ctype == xlrd.XL_CELL_BOOLEAN:
        return "TRUE" if cell.value else "FALSE"
    return str(cell.value).strip()


# PowerPoint 97-2003 ([MS-PPT]): records, some of which hold text.
_PPT_TEXT_CHARS, _PPT_TEXT_BYTES = 0x0FA0, 0x0FA8
_PPT_SLIDE_LIST, _PPT_SLIDE_PERSIST = 0x0FF0, 0x03F3
_PPT_SKIP = {0x03F8, 0x0FC9, 0x0FD9}  # masters, handouts, the outline view's placeholders
_PPT_CRYPT = 0x2F14


def _ppt_text(ole) -> str:
    if ole.exists("EncryptedSummary"):
        raise SkipFile("password")
    data = ole.openstream("PowerPoint Document").read()
    parts: list[str] = []
    count = {"slides": 0, "chars": 0}

    def walk(start: int, end: int, depth: int, in_slide_list: bool):
        pos = start
        while pos + 8 <= end and count["chars"] <= MAX_CHARS:
            ver_inst, rtype, rlen = struct.unpack_from("<HHI", data, pos)
            body, nxt = pos + 8, pos + 8 + rlen
            if nxt > end:
                return
            if rtype == _PPT_CRYPT:
                raise SkipFile("password")
            if ver_inst & 0xF == 0xF:  # a container
                instance = ver_inst >> 4
                if rtype == _PPT_SLIDE_LIST and instance == 1:  # the masters' list
                    pass
                elif rtype not in _PPT_SKIP and depth < 32:
                    walk(body, nxt, depth + 1, rtype == _PPT_SLIDE_LIST and instance == 0)
            elif rtype == _PPT_SLIDE_PERSIST and in_slide_list:
                count["slides"] += 1
                parts.append(f"\n\nSlide {count['slides']}:")
            elif rtype in (_PPT_TEXT_CHARS, _PPT_TEXT_BYTES):
                raw = data[body:nxt]
                text = raw.decode("utf-16-le", "replace") if rtype == _PPT_TEXT_CHARS else raw.decode("latin-1")
                text = text.replace("\r", "\n").replace("\x0b", "\n").strip()
                if text and (not parts or parts[-1] != text):
                    parts.append(text)
                    count["chars"] += len(text)
            pos = nxt

    walk(0, len(data), 0, False)
    return "\n".join(parts)


# ---- OpenDocument (LibreOffice, OpenOffice) --------------------------------------------

_TEXT_NS = "urn:oasis:names:tc:opendocument:xmlns:text:1.0"
_TABLE_NS = "urn:oasis:names:tc:opendocument:xmlns:table:1.0"
_DRAW_NS = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
_OFFICE_NS = "urn:oasis:names:tc:opendocument:xmlns:office:1.0"
_P, _H = f"{{{_TEXT_NS}}}p", f"{{{_TEXT_NS}}}h"
_SPACE, _TAB, _BREAK = f"{{{_TEXT_NS}}}s", f"{{{_TEXT_NS}}}tab", f"{{{_TEXT_NS}}}line-break"
_SPACE_COUNT = f"{{{_TEXT_NS}}}c"
_CHANGES = f"{{{_TEXT_NS}}}tracked-changes"  # earlier versions of changed text
_CELL, _ROW, _TABLE = f"{{{_TABLE_NS}}}table-cell", f"{{{_TABLE_NS}}}table-row", f"{{{_TABLE_NS}}}table"
_TABLE_NAME = f"{{{_TABLE_NS}}}name"
_PAGE, _PAGE_NAME = f"{{{_DRAW_NS}}}page", f"{{{_DRAW_NS}}}name"
_SPREADSHEET, _PRESENTATION = f"{{{_OFFICE_NS}}}spreadsheet", f"{{{_OFFICE_NS}}}presentation"


def read_odf(path: Path) -> str:
    if path.suffix.lower().startswith(".fod"):  # flat: one XML file
        with path.open("rb") as f:
            return _odf_text(f)
    with zipfile.ZipFile(path) as z:
        names = set(z.namelist())
        if "META-INF/manifest.xml" in names:
            with z.open("META-INF/manifest.xml") as m:
                if b"encryption-data" in m.read(4 * 1024 * 1024):
                    raise SkipFile("password")
        if "content.xml" not in names:
            raise Damaged("document")
        with z.open("content.xml") as f:
            return _odf_text(f)


def _inline(el) -> str:
    """A paragraph's text with its spaces, tabs and line breaks (paragraphs inside it, like
    footnotes, are read on their own)."""
    parts = [el.text or ""]
    for child in el:
        tag = child.tag
        if tag == _SPACE:
            try:
                parts.append(" " * min(int(child.get(_SPACE_COUNT, "1")), 100))
            except ValueError:
                parts.append(" ")
        elif tag == _TAB:
            parts.append("\t")
        elif tag == _BREAK:
            parts.append("\n")
        elif tag not in (_P, _H):
            parts.append(_inline(child))
        parts.append(child.tail or "")
    return "".join(parts)


def _odf_text(f) -> str:
    out: list[str] = []
    size = 0
    rows = 0
    mode = "text"
    cells: list[list[str]] = []  # per open table cell: its paragraphs
    row: list[list[str]] = []  # per open table row: its cells
    skipping = 0  # inside tracked changes
    paragraphs = 0  # open paragraphs (they can nest, e.g. footnotes)
    pages = 0
    try:
        for event, el in ET.iterparse(f, events=("start", "end")):
            tag = el.tag
            if event == "start":
                if tag == _CHANGES:
                    skipping += 1
                elif tag in (_P, _H):
                    paragraphs += 1
                elif tag == _CELL:
                    cells.append([])
                elif tag == _ROW:
                    row.append([])
                elif tag == _SPREADSHEET:
                    mode = "sheet"
                elif tag == _PRESENTATION:
                    mode = "slides"
                elif tag == _TABLE and mode == "sheet" and not cells:
                    out.append(f"\n\nSheet {el.get(_TABLE_NAME, '')}:")
                elif tag == _PAGE:
                    pages += 1
                    out.append(f"\n\n{'Slide' if mode == 'slides' else 'Page'} {pages}:")
                continue
            if tag == _CHANGES:
                skipping -= 1
            elif tag in (_P, _H):
                paragraphs -= 1
                if not skipping:
                    text = _inline(el).strip()
                    if text:
                        (cells[-1] if cells else out).append(text)
                        size += len(text)
                if not paragraphs:
                    el.clear()
            elif tag == _CELL and cells:
                text = " ".join(cells.pop())
                if row and text:
                    row[-1].append(text)
                elif text:
                    out.append(text)
            elif tag == _ROW and row:
                values = row.pop()
                if values:
                    (cells[-1] if cells else out).append(" | ".join(values))
                    rows += 1
                el.clear()
            if size > MAX_CHARS or rows > MAX_ROWS:
                break
    except ET.ParseError:
        if not out:
            raise Damaged("document") from None
    return "\n".join(out)


def odf_thumbnail(path: Path) -> bytes | None:
    """The preview picture LibreOffice saves into every document."""
    return _zip_picture(path, ("Thumbnails/thumbnail.png",))


def _zip_picture(path: Path, names) -> bytes | None:
    try:
        with zipfile.ZipFile(path) as z:
            for name in names:
                try:
                    info = z.getinfo(name)
                except KeyError:
                    continue
                if info.file_size <= 4 * 1024 * 1024:
                    return z.read(info)
    except (OSError, zipfile.BadZipFile):
        pass
    return None


# ---- RTF ------------------------------------------------------------------------------

# Groups whose text isn't part of the document: fonts, styles, pictures, field codes, …
_RTF_SKIP = set("""
aftncn aftnsep aftnsepc annotation atnauthor atndate atnicn atnid atnparent atnref atntime
atrfend atrfstart author background bkmkend bkmkstart blipuid buptim category colorschememapping
colortbl comment company creatim datafield datastore defchp defpap do doccomm docvar dptxbxtext
ebcend ebcstart factoidname falt fchars ffdeftext ffentrymcr ffexitmcr ffformat ffhelptext ffl
ffname ffstattext file filetbl fldinst fldtype fname fontemb fontfile fonttbl footer footerf
footerl footerr formfield ftncn ftnsep ftnsepc g generator gridtbl header headerf headerl headerr
hl hlfr hlinkbase hlloc hlsrc hsv htmltag info keycode keywords latentstyles lchars
levelnumbers leveltext lfolevel linkval list listlevel listname listoverride listoverridetable
listpicture liststylename listtable listtext lsdlockedexcept macc maccPr mailmerge maln malnScr
manager margPr mbar mbarPr mbaseJc mbegChr mborderBox mborderBoxPr mbox mboxPr mchr mcount mctrlPr
md mdeg mdegHide mden mdiff mdPr me mendChr meqArr meqArrPr mf mfName mfPr mfunc mfuncPr mgroupChr
mgroupChrPr mgrow mhideBot mhideLeft mhideRight mhideTop mhtmltag mlim mlimloc mlimlow mlimlowPr
mlimupp mlimuppPr mm mmaddfieldname mmath mmathPict mmathPr mmaxdist mmc mmcJc mmconnectstr
mmconnectstrdata mmcPr mmcs mmdatasource mmheadersource mmmailsubject mmodso mmodsofilter
mmodsofldmpdata mmodsomappedname mmodsoname mmodsorecipdata mmodsosort mmodsosrc mmodsotable
mmodsoudl mmodsoudldata mmodsouniquetag mmPr mmquery mmr mnary mnaryPr mnoBreak mnum mobjDist
moMath moMathPara moMathParaPr mopEmu mphant mphantPr mplcHide mpos mr mrad mradPr mrPr msepChr
mshow mshp msPre msPrePr msSub msSubPr msSubSup msSubSupPr msSup msSupPr mstrikeBLTR mstrikeH
mstrikeTLBR mstrikeV msub msubHide msup msupHide mtransp mtype mvertJc mvfmf mvfml mvtof mvtol
mzeroAsc mzeroDesc mzeroWid nesttableprops nextfile nonesttables nonshppict objalias objclass
objdata object objname objsect objtime oldcprops oldpprops oldsprops oldtprops oleclsid operator
panose password passwordhash pgp pgptbl picprop pict pn pnseclvl pntext pntxta pntxtb printim
private propname protend protstart protusertbl pxe result revtbl revtim rsidtbl rxe shp shpgrp
shpinst shppict shprslt shptxt sn sp staticval stylesheet subject sv svb tc template themedata
title txe ud upr userprops wgrffmtfilter windowcaption writereservation writereservhash xe xform
xmlattrname xmlattrvalue xmlclose xmlname xmlnstbl xmlopen
""".split())
_RTF_CHARS = {
    "par": "\n", "sect": "\n\n", "page": "\n\n", "line": "\n", "tab": "\t", "cell": " | ",
    "nestcell": " | ", "row": "\n", "nestrow": "\n", "emdash": "\u2014", "endash": "\u2013",
    "emspace": " ", "enspace": " ", "qmspace": " ", "bullet": "\u2022", "lquote": "\u2018",
    "rquote": "\u2019", "ldblquote": "\u201c", "rdblquote": "\u201d",
}
_RTF_TOKEN = re.compile(
    r"\\([a-zA-Z]{1,32})(-?\d{1,10})? ?|\\'([0-9a-fA-F]{2})|\\([^a-zA-Z])|([{}])|[\r\n]+|([^\\{}\r\n]+)")


def read_rtf(path: Path) -> str:
    return rtf_text(path.read_bytes().decode("latin-1"))


def rtf_text(rtf: str) -> str:
    """RTF's text without its markup. (The file is read as Latin-1, one character per byte;
    RTF itself is ASCII, with other characters escaped in the document's code page.)"""
    out: list[str] = []
    pending = bytearray()  # \'hh bytes, decoded together (a character can take two)
    stack: list[tuple[int, bool]] = []
    ignorable, uc, skip = False, 1, 0
    codepage = "cp1252"
    pos, n = 0, len(rtf)

    def flush():
        if pending:
            out.append(pending.decode(codepage, "replace"))
            pending.clear()

    while pos < n:
        m = _RTF_TOKEN.match(rtf, pos)
        if not m:
            pos += 1
            continue
        pos = m.end()
        word, arg, hexcode, symbol, brace, plain = m.groups()
        if hexcode:
            if skip:
                skip -= 1
            elif not ignorable:
                pending.append(int(hexcode, 16))
            continue
        flush()
        if brace:
            skip = 0
            if brace == "{":
                stack.append((uc, ignorable))
            elif stack:
                uc, ignorable = stack.pop()
        elif symbol:
            skip = 0
            if symbol == "*":
                ignorable = True
            elif not ignorable:
                if symbol == "~":
                    out.append("\xa0")
                elif symbol in "{}\\":
                    out.append(symbol)
                elif symbol in "\r\n":  # a backslash before a line break is a paragraph mark
                    out.append("\n")
                elif symbol == "_":
                    out.append("-")
        elif word:
            skip = 0
            if word in _RTF_SKIP:
                ignorable = True
            elif word == "bin" and arg:  # raw binary data follows
                pos += max(0, int(arg))
            elif word == "ansicpg" and arg:
                cp = f"cp{arg}"
                try:
                    "".encode(cp)
                    codepage = cp
                except LookupError:
                    pass
            elif ignorable:
                pass
            elif word == "uc" and arg:
                uc = max(0, int(arg))
            elif word == "u" and arg:
                code = int(arg)
                out.append(chr(code + 0x10000 if code < 0 else code))
                skip = uc
            elif word in _RTF_CHARS:
                out.append(_RTF_CHARS[word])
        elif plain:
            if skip:
                taken = min(skip, len(plain))
                skip -= taken
                plain = plain[taken:]
            if plain and not ignorable:
                out.append(plain)
        if len(out) > 2 * MAX_CHARS:
            break
    flush()
    # \u gives UTF-16 code units: put surrogate pairs back together.
    return "".join(out).encode("utf-16", "surrogatepass").decode("utf-16", "replace")
