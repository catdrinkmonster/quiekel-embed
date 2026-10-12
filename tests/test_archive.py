import base64
import gzip
import io
import os
import subprocess
import tarfile
import zipfile

import pytest

import samples
from quiekel_embed import archive


def make_zip(path, files):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files.items():
            z.writestr(name, data)
    return str(path)


def zip_bytes(files):
    buf = io.BytesIO()
    make_zip(buf, files)
    return buf.getvalue()


def walk(path, **kw):
    """What's inside, path -> why it can't be searched (None: it can)."""
    return {i.path: i.problem for i in archive.walk(str(path), 0.0, lambda name: True, **kw)}


def test_lists_files_as_if_the_archive_were_a_folder(tmp_path):
    z = make_zip(tmp_path / "Urlaub.zip", {
        "2019/strand.png": samples.png(), "notes.txt": "Sonnencreme kaufen", "empty/": "",
        "../evil.txt": "outside", "C:/abs.txt": "drive letter",
    })
    assert walk(z) == {z + "\\2019\\strand.png": None, z + "\\notes.txt": None,
                       z + "\\../evil.txt": "odd_name", z + "\\C:/abs.txt": "odd_name"}  # no folders, no tricks
    assert archive.split(z + "\\2019\\strand.png") == (z, ["2019/strand.png"])
    assert archive.split(str(tmp_path / "plain" / "file.txt")) is None
    assert archive.exists(z + "\\notes.txt")


def test_archives_inside_archives_are_looked_into_once(tmp_path):
    deepest = zip_bytes({"deep.txt": "ganz unten"})
    z = make_zip(tmp_path / "outer.zip", {"innen/archiv.zip": zip_bytes({"brief.txt": "Hallo", "noch.zip": deepest})})
    inner = z + "\\innen\\archiv.zip"
    found = walk(z)
    assert found[inner + "\\brief.txt"] is None
    assert found[inner + "\\noch.zip"] == "nested_deep"
    assert inner + "\\noch.zip\\deep.txt" not in found
    assert archive.split(inner + "\\brief.txt") == (z, ["innen/archiv.zip", "brief.txt"])
    assert archive.read(inner + "\\brief.txt", 100) == b"Hallo"
    assert walk(z, nested=False)[inner] == "nested_off"


def test_tar_files_and_single_compressed_files(tmp_path):
    tgz = tmp_path / "backup.tar.gz"
    with tarfile.open(tgz, "w:gz") as t:
        info = tarfile.TarInfo("docs/steuer.txt")
        info.size = 11
        t.addfile(info, io.BytesIO(b"Steuer 2024"))
    assert walk(tgz) == {f"{tgz}\\docs\\steuer.txt": None}
    assert archive.read(f"{tgz}\\docs\\steuer.txt", 100) == b"Steuer 2024"
    gz = tmp_path / "log.txt.gz"
    gz.write_bytes(gzip.compress(b"ein Logbuch"))
    assert [(i.path, i.size) for i in archive.walk(str(gz), 0.0, lambda n: True)] == [(f"{gz}\\log.txt", 11)]
    assert archive.read(f"{gz}\\log.txt", 100) == b"ein Logbuch"


@pytest.mark.skipif(archive._library() is None, reason="needs Windows' archive library (Windows 11, 2023 and later)")
def test_7z_through_windows_own_archive_library(tmp_path):
    work = tmp_path / "work"
    work.mkdir()
    (work / "rezept.txt").write_text("Apfelkuchen", encoding="utf-8")
    (work / "zweites.txt").write_text("Zweite Datei", encoding="utf-8")
    seven = tmp_path / "rezepte.7z"
    tar_exe = os.path.join(os.environ["SystemRoot"], "System32", "tar.exe")
    subprocess.run([tar_exe, "-a", "-cf", str(seven), "rezept.txt", "zweites.txt"], cwd=work, check=True)
    assert set(walk(seven)) == {f"{seven}\\rezept.txt", f"{seven}\\zweites.txt"}
    with archive.session():  # in any order
        assert archive.read(f"{seven}\\zweites.txt", 100) == b"Zweite Datei"
        assert archive.read(f"{seven}\\rezept.txt", 100) == b"Apfelkuchen"
        assert archive._open_now
    assert not archive._open_now  # nothing kept open afterwards (Windows would lock the file)
    in_zip = make_zip(tmp_path / "mit7z.zip", {"rezepte.7z": seven.read_bytes()})
    assert archive.read(in_zip + "\\rezepte.7z\\rezept.txt", 100) == b"Apfelkuchen"


def test_a_broken_archive_is_reported(tmp_path):
    bad = tmp_path / "broken.zip"
    bad.write_bytes(b"not a zip at all")
    assert walk(bad) == {str(bad): "damaged_archive"}


def test_password_protected_entries_and_very_big_archives(tmp_path):
    data = bytearray(zip_bytes({"geheim.txt": "x", "offen.txt": "y"}))
    for header, flags_at in ((b"PK\x03\x04", 6), (b"PK\x01\x02", 8)):  # mark the first entry encrypted
        at = data.find(header)
        data[at + flags_at] |= 0x1
    z = tmp_path / "teils.zip"
    z.write_bytes(bytes(data))
    assert walk(z) == {f"{z}\\geheim.txt": "encrypted", f"{z}\\offen.txt": None}
    big = make_zip(tmp_path / "backup.zip", {f"f{i}.txt": "x" for i in range(5)})
    assert walk(big, max_entries=4) == {big: "too_many"}


def test_names_from_windows_own_zip_folders(tmp_path):
    if archive._OEM not in ("cp437", "cp850"):
        pytest.skip("another OEM code page")
    data = zip_bytes({"MXrz.txt": "Rechnung"}).replace(b"MXrz.txt", b"M\x84rz.txt")  # "März" in cp437/850
    z = tmp_path / "rechnungen.zip"
    z.write_bytes(data)
    assert walk(z) == {f"{z}\\M\u00e4rz.txt": None}
    assert archive.read(f"{z}\\M\u00e4rz.txt", 100) == b"Rechnung"


def test_email_attachments_are_files_inside_the_email(tmp_path):
    eml = tmp_path / "Unterlagen.eml"
    eml.write_bytes(
        b"From: a@example.com\r\nSubject: Anhang\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n"
        b"--B\r\nContent-Type: multipart/related; boundary=R\r\n\r\n"
        b"--R\r\nContent-Type: text/html\r\n\r\n<p>Hallo <img src=\"cid:logo1\"></p>\r\n"
        b"--R\r\nContent-Type: image/png\r\nContent-ID: <logo1>\r\nContent-Disposition: inline; filename=logo.png\r\n"
        b"Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R--\r\n"
        b"--B\r\nContent-Type: application/zip\r\nContent-Disposition: attachment; filename=\"vertrag.zip\"\r\n"
        b"Content-Transfer-Encoding: base64\r\n\r\n" + base64.encodebytes(zip_bytes({"vertrag.txt": "Vertrag"}))
        + b"\r\n--B--\r\n")
    assert walk(eml) == {f"{eml}\\logo.png": "inline_image", f"{eml}\\vertrag.zip": None,
                         f"{eml}\\vertrag.zip\\vertrag.txt": None}
    assert archive.read(f"{eml}\\vertrag.zip\\vertrag.txt", 100) == b"Vertrag"
    msg = tmp_path / "Angebot.msg"
    msg.write_bytes(samples.outlook_msg("Angebot", "Siehe Anhang", attachments=[("angebot.txt", b"Preis " * 1000)]))
    assert walk(msg) == {f"{msg}\\angebot.txt": None}
    assert archive.read(f"{msg}\\angebot.txt", 10_000) == b"Preis " * 1000


def test_reading_refuses_more_than_the_limit(tmp_path):
    z = make_zip(tmp_path / "big.zip", {"big.txt": "x" * 5000})
    assert archive.read(z + "\\big.txt", 10_000) == b"x" * 5000
    with pytest.raises(ValueError):
        archive.read(z + "\\big.txt", 1000)


def test_opening_copies_just_that_file_to_a_temp_folder(tmp_path, monkeypatch):
    monkeypatch.setattr(archive, "OPENED", tmp_path / "opened")
    z = make_zip(tmp_path / "docs.zip", {"a/b/letter.txt": "Hallo"})
    copy = archive.open_copy(z + "\\a\\b\\letter.txt", 1000)
    assert copy.read_text() == "Hallo"
    assert copy.is_relative_to(tmp_path / "opened")
    with archive.extracted(z + "\\a\\b\\letter.txt", 1000) as tmp:
        assert tmp.name == "letter.txt" and tmp.read_text() == "Hallo"
    assert not tmp.exists()  # temporary copies are cleaned up
