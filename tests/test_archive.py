import io
import zipfile

import pytest
from PIL import Image

from quiekel_embed import archive


def make_zip(path, files):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files.items():
            z.writestr(name, data)
    return str(path)


def png_bytes():
    buf = io.BytesIO()
    Image.new("RGB", (200, 150), (240, 120, 40)).save(buf, "PNG")
    return buf.getvalue()


def test_lists_files_as_if_the_archive_were_a_folder(tmp_path):
    z = make_zip(tmp_path / "Urlaub.zip", {
        "2019/strand.png": png_bytes(), "notes.txt": "Sonnencreme kaufen", "empty/": "",
        "../evil.txt": "outside", "C:/abs.txt": "drive letter",
    })
    found = {p: size for p, parts, size, mtime in archive.entries(z)}
    assert set(found) == {z + "\\2019\\strand.png", z + "\\notes.txt"}  # no folders, no tricks
    assert archive.split(z + "\\2019\\strand.png") == (z, "2019/strand.png")
    assert archive.split(str(tmp_path / "plain" / "file.txt")) is None
    assert archive.exists(z + "\\notes.txt")


def test_reading_refuses_more_than_the_limit(tmp_path):
    z = make_zip(tmp_path / "big.zip", {"big.txt": "x" * 5000})
    assert archive.read(z + "\\big.txt", 10_000) == b"x" * 5000
    with pytest.raises(ValueError):
        archive.read(z + "\\big.txt", 1000)


def test_a_broken_archive_is_skipped(tmp_path):
    bad = tmp_path / "broken.zip"
    bad.write_bytes(b"not a zip at all")
    assert list(archive.entries(str(bad))) == []


def test_opening_copies_just_that_file_to_a_temp_folder(tmp_path, monkeypatch):
    monkeypatch.setattr(archive, "OPENED", tmp_path / "opened")
    z = make_zip(tmp_path / "docs.zip", {"a/b/letter.txt": "Hallo"})
    copy = archive.open_copy(z + "\\a\\b\\letter.txt", 1000)
    assert copy.read_text() == "Hallo"
    assert copy.is_relative_to(tmp_path / "opened")
    with archive.extracted(z + "\\a\\b\\letter.txt", 1000) as tmp:
        assert tmp.name == "letter.txt" and tmp.read_text() == "Hallo"
    assert not tmp.exists()  # temporary copies are cleaned up
