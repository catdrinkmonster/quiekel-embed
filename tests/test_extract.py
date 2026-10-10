import io
import zipfile
from pathlib import Path

import pytest
from PIL import Image, UnidentifiedImageError

from quiekel_embed import config
from quiekel_embed.extract import SkipFile, chunk_text, extract, kind_of, problem


def test_kind_of():
    assert kind_of(Path("a.PDF")) == "doc"
    assert kind_of(Path("a.py")) == "code"
    assert kind_of(Path("notes.md")) == "text"
    assert kind_of(Path("IMG_1.HEIC")) == "image"
    assert kind_of(Path("movie.mkv")) is None


def test_short_text_is_one_chunk():
    assert chunk_text("hello   world\n\n\n\nbye") == ["hello world\n\nbye"]


def test_windows_newlines_and_indentation():
    code = "def f():\r\n    return  1   \r\n\r\n\r\n\r\nx = 2\r\n"
    assert chunk_text(code) == ["def f():\n    return 1\n\nx = 2"]


def test_long_text_chunks_cover_everything_with_overlap():
    words = [f"word{i}" for i in range(3000)]
    text = " ".join(words)
    chunks = chunk_text(text)
    assert len(chunks) > 1
    assert all(len(c) <= config.CHUNK_CHARS for c in chunks)
    joined = " ".join(chunks)
    assert all(w in joined for w in words)  # nothing dropped
    # consecutive chunks overlap
    for a, b in zip(chunks, chunks[1:]):
        assert b.split()[0] in a


def test_text_file_gets_title_prefix(tmp_path):
    p = tmp_path / "groceries.md"
    p.write_text("Milk, eggs, bread", encoding="utf-8")
    ex = extract(p, p.stat().st_size)
    assert ex.kind == "text"
    assert ex.chunks[0].text == "title: groceries.md | text: Milk, eggs, bread"
    assert ex.chunks[0].snippet == "Milk, eggs, bread"


def test_binary_and_empty_files_are_skipped(tmp_path):
    b = tmp_path / "data.json"
    b.write_bytes(b"\x00\x01\x02")
    with pytest.raises(SkipFile):
        extract(b, 3)
    e = tmp_path / "empty.txt"
    e.write_text("   \n ")
    with pytest.raises(SkipFile):
        extract(e, 5)


def test_images(tmp_path):
    big = tmp_path / "photo.png"
    Image.new("RGBA", (3000, 2000), (255, 0, 0, 128)).save(big)
    ex = extract(big, big.stat().st_size)
    img = ex.chunks[0].image
    assert ex.kind == "image" and img.mode == "RGB"
    assert max(img.size) == config.MAX_IMAGE_SIDE

    icon = tmp_path / "icon.png"
    Image.new("RGB", (32, 32)).save(icon)
    with pytest.raises(SkipFile):
        extract(icon, icon.stat().st_size)


def test_docx_and_pdf(tmp_path):
    import docx
    import pymupdf

    d = docx.Document()
    d.add_paragraph("Quarterly budget review")
    dp = tmp_path / "budget.docx"
    d.save(dp)
    assert "Quarterly budget review" in extract(dp, dp.stat().st_size).chunks[0].snippet

    pdf = pymupdf.open()
    pdf.new_page().insert_text((72, 72), "Rental agreement for the flat")
    pp = tmp_path / "lease.pdf"
    pdf.save(pp)
    assert "Rental agreement" in extract(pp, pp.stat().st_size).chunks[0].snippet

    scanned = pymupdf.open()
    scanned.new_page()  # no text layer
    sp = tmp_path / "scan.pdf"
    scanned.save(sp)
    ex = extract(sp, sp.stat().st_size)
    assert ex.chunks[0].image is not None and ex.chunks[0].snippet.startswith("Page 1")


def jpeg_bytes(size=(400, 300)):
    buf = io.BytesIO()
    Image.new("RGB", size, (30, 120, 200)).save(buf, "JPEG", quality=90)
    return buf.getvalue()


def test_a_photo_missing_its_last_bytes_is_still_indexed(tmp_path):
    p = tmp_path / "cut.jpg"
    p.write_bytes(jpeg_bytes()[:-4])  # an interrupted copy
    ex = extract(p, p.stat().st_size)
    assert ex.kind == "image" and ex.chunks[0].image.size == (400, 300)


def test_empty_pictures_are_skipped_quietly(tmp_path):
    p = tmp_path / "empty.jpg"
    p.write_bytes(b"")
    with pytest.raises(SkipFile):
        extract(p, 0)


def test_an_archive_named_like_a_picture_is_skipped_not_an_error(tmp_path):
    p = tmp_path / "sticker.webp"  # e.g. an animated sticker: a ZIP with an animation inside
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("animation/animation.json", "{}")
    with pytest.raises(SkipFile):
        extract(p, p.stat().st_size)


def test_a_broken_picture_is_still_reported(tmp_path):
    p = tmp_path / "stub.jpg"
    p.write_bytes(b"\xff\xd8\xff")
    with pytest.raises(UnidentifiedImageError) as e:
        extract(p, 3)
    assert problem(e.value) == "!damaged_image"


def test_problems_are_named_without_file_paths():
    assert problem(PermissionError(13, "denied")) == "!no_access"
    assert problem(zipfile.BadZipFile("File is not a zip file")) == "!damaged_document"
    text = problem(ValueError("cannot read 'C:\\Users\\me\\Documents\\secret.txt' at all"))
    assert "Users" not in text and text.startswith("ValueError: cannot read")
