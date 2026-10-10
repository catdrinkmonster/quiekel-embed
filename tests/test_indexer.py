import threading
import time
from types import SimpleNamespace

import numpy as np
import pytest

from quiekel_embed import config


class FakeEmbedder:
    """Stands in for the model: every text and image becomes the same unit vector."""

    def __init__(self, on_gpu=False):
        self.ready, self.cuda, self.on_gpu = True, on_gpu, on_gpu
        self.pace, self.batch_scale, self.released = None, 1.0, []
        self.last_used, self.unloaded, self.loads = time.monotonic(), [], 0
        self.status = "ready"

    def unload(self, why):
        self.unloaded.append(why)
        self.ready, self.status = False, "asleep"

    def load(self, prefer_gpu=True):
        self.loads += 1
        self.ready, self.status = True, "ready"

    def documents(self, texts):
        return np.ones((len(texts), config.EMBED_DIM), np.float32) / config.EMBED_DIM ** 0.5

    def images(self, images):
        return np.ones((len(images), config.EMBED_DIM), np.float32) / config.EMBED_DIM ** 0.5

    def release_gpu(self, why):
        self.released.append(why)
        self.on_gpu = False

    def use_gpu(self):
        self.on_gpu = True
        return True

    def free_cache(self):
        pass


class FakeGovernor:
    def __init__(self):
        self.decision = SimpleNamespace(duty=1.0, level="full", code="full", params={})
        self.metrics = SimpleNamespace(ram_free_gb=16.0, vram_free_gb=None, fullscreen=False)

    def pace(self, *args, **kwargs):
        pass


@pytest.fixture
def make_indexer(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "DATA_DIR", tmp_path / "data")
    monkeypatch.setattr(config, "DB_PATH", tmp_path / "data" / "state.db")
    monkeypatch.setattr(config, "VECTORS_DIR", tmp_path / "data" / "vectors")
    from quiekel_embed.indexer import Indexer
    from quiekel_embed.store import Settings, Store

    def make(embedder=None):
        store = Store()
        return Indexer(store, embedder or FakeEmbedder(), FakeGovernor(), Settings(store))
    return make


def notes(folder, n=40):
    folder.mkdir()
    files = []
    for i in range(n):
        p = folder / f"note_{i:02d}.txt"
        p.write_text(f"note number {i} about apples" if i % 7 else "", encoding="utf-8")  # some empty: skipped
        st = p.stat()
        files.append((str(p), st.st_size, st.st_mtime))
    return files


def in_thread(fn):
    t = threading.Thread(target=fn, daemon=True)
    t.start()
    return t


def test_reading_ahead_indexes_every_file_in_order(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder)
    fid = indexer.store.add_folder(str(folder))

    indexer._index_files(fid, str(folder), files)

    saved = indexer.store.file_signatures(fid)
    assert sorted(saved) == sorted(f[0] for f in files)
    assert indexer.progress()["done"] == 40
    assert len(indexer.store.file_ids_under(str(folder))) == 34  # the 6 empty notes are skipped


def test_waiting_out_a_game_hands_the_gpu_back(make_indexer, tmp_path):
    indexer = make_indexer(FakeEmbedder(on_gpu=True))
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = indexer.store.add_folder(str(folder))
    gov = indexer.governor
    gov.decision.duty, gov.metrics.fullscreen = 0.0, True  # a game is running: everything waits

    t = in_thread(lambda: indexer._index_files(fid, str(folder), files))
    for _ in range(50):
        if indexer.embedder.released:
            break
        time.sleep(0.1)
    assert indexer.embedder.released, "the model kept its video memory during the game"

    gov.decision.duty, gov.metrics.fullscreen = 1.0, False  # the game is over
    t.join(10)
    assert not t.is_alive()
    assert indexer.embedder.on_gpu
    assert len(indexer.store.file_signatures(fid)) == 10


def test_files_inside_zips_are_indexed_like_a_folder(make_indexer, tmp_path):
    import zipfile

    indexer = make_indexer()
    folder = tmp_path / "docs"
    folder.mkdir()
    (folder / "plain.txt").write_text("a note outside the archive", encoding="utf-8")
    zpath = folder / "Steuer 2024.zip"
    with zipfile.ZipFile(zpath, "w") as z:
        z.writestr("Belege/quittung.txt", "Quittung Laptop 899 Euro")
        z.writestr("Belege/brief.txt", "Brief vom Finanzamt")
        z.writestr("tool.exe", "binary, not indexed")
    fid = indexer.store.add_folder(str(folder))

    indexer._scan_folder(fid)
    inner = str(zpath) + "\\Belege\\quittung.txt"
    paths = set(indexer.store.file_signatures(fid))
    assert paths == {str(folder / "plain.txt"), inner, str(zpath) + "\\Belege\\brief.txt"}
    assert len(indexer.store.file_ids_under(str(zpath))) == 2  # the archive works as a folder scope

    # The archive changes: one file gone, one new. The watcher brings the index in line.
    with zipfile.ZipFile(zpath, "w") as z:
        z.writestr("Belege/quittung.txt", "Quittung Laptop 899 Euro")
        z.writestr("Belege/bescheid.txt", "Steuerbescheid 2024")
    indexer._handle_paths([str(zpath)])
    paths = set(indexer.store.file_signatures(fid))
    assert str(zpath) + "\\Belege\\bescheid.txt" in paths and str(zpath) + "\\Belege\\brief.txt" not in paths

    # Switched off in Settings -> File types: the next scan forgets what's inside ZIP files.
    indexer.settings.update(types_off=[".zip"])
    indexer._scan_folder(fid)
    assert set(indexer.store.file_signatures(fid)) == {str(folder / "plain.txt")}


def test_found_files_are_searchable_by_name_before_they_are_indexed(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 3)
    fid = indexer.store.add_folder(str(folder))
    indexer.store.add_pending(fid, [(p, size, mtime, "text", "docs note_01.txt") for p, size, mtime in files[1:2]])

    pending_id, _, _, status = indexer.store.file_signatures(fid)[files[1][0]]
    assert status == "pending"
    hits = indexer.store.keyword_search('"note_01"*', 10)
    assert [h["file_id"] for h in hits] == [pending_id]
    assert indexer.store.folders()[0]["pending"] == 1

    indexer._scan_folder(fid)  # the scan picks up unfinished files
    assert "pending" not in {v[3] for v in indexer.store.file_signatures(fid).values()}  # all done
    assert indexer.store.folders()[0]["pending"] == 0


def test_files_that_failed_are_tried_again_on_the_next_scan(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 2)
    fid = indexer.store.add_folder(str(folder))
    path, size, mtime = files[1]
    indexer.store.save_files([{"folder_id": fid, "path": path, "size": size, "mtime": mtime, "kind": "text",
                               "status": "error", "chunks": 0, "error": "!damaged_document"}], [])
    indexer._scan_folder(fid)
    assert indexer.store.file_signatures(fid)[path][3] == "indexed"


def test_a_failing_reader_cannot_hang_indexing(make_indexer, tmp_path, monkeypatch):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = indexer.store.add_folder(str(folder))
    read = indexer._read

    def flaky(folder_id, root, path, *rest):
        if path.endswith("note_05.txt"):
            raise MemoryError("out of memory while reading")
        return read(folder_id, root, path, *rest)

    monkeypatch.setattr(indexer, "_read", flaky)
    t = in_thread(lambda: indexer._index_files(fid, str(folder), files))
    t.join(10)
    assert not t.is_alive()


def test_lean_memory_lets_the_model_go_when_unused_and_wakes_it_for_work(make_indexer):
    e = FakeEmbedder()
    ix = make_indexer(e)
    e.last_used = ix._last_work = time.monotonic() - 3600  # nothing for an hour
    ix._manage_device(working=False)
    assert e.unloaded == []  # "fast" (the default) keeps it ready
    ix.settings.update(memory="lean")
    e.last_used = time.monotonic()  # just searched
    ix._manage_device(working=False)
    assert e.unloaded == []
    e.last_used = time.monotonic() - 3600
    ix._manage_device(working=False)
    assert e.unloaded and not e.ready
    ix._ensure_model()  # a new file to index
    assert e.ready and e.loads == 1


def test_lean_memory_lets_the_model_go_entirely_for_a_game(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    ix.governor.metrics.fullscreen, ix.governor.metrics.vram_free_gb = True, 4.0
    ix._manage_device(working=False)
    assert e.released and not e.unloaded  # fast: into RAM, ready for the next search
    e.on_gpu, e.released = True, []
    ix.settings.update(memory="lean")
    ix._manage_device(working=False)
    assert e.unloaded and not e.released  # lean: out of memory altogether


def hide(path):
    import ctypes

    ctypes.windll.kernel32.SetFileAttributesW(str(path), 0x2)  # FILE_ATTRIBUTE_HIDDEN


def test_every_file_left_out_comes_with_its_reason(make_indexer, tmp_path, monkeypatch):
    import zipfile

    from quiekel_embed import archive

    indexer = make_indexer()
    folder = tmp_path / "docs"
    (folder / "node_modules" / "lib").mkdir(parents=True)
    (folder / "node_modules" / "lib" / "index.js").write_text("module.exports = 1", encoding="utf-8")
    for name in ("brief.txt", ".env", "~$brief.docx", "urlaub.mp4", "setup.exe", "daten.xyz", "postfach.pst"):
        (folder / name).write_bytes(b"x" * 10)
    (folder / "versteckt.txt").write_text("hidden", encoding="utf-8")
    hide(folder / "versteckt.txt")
    with zipfile.ZipFile(folder / "backup.zip", "w") as z:
        for i in range(5):
            z.writestr(f"f{i}.txt", "x")
    monkeypatch.setattr(archive, "MAX_ENTRIES", 4)
    fid = indexer.store.add_folder(str(folder))

    indexer._scan_folder(fid)
    assert set(indexer.store.file_signatures(fid)) == {str(folder / "brief.txt")}
    report = indexer.store.details(fid)["report"]
    assert {k: v["n"] for k, v in report.items()} == {
        "program_folder": 1, "hidden": 2, "office_temp": 1, "media": 1, "program": 1, "type": 1, "mailbox": 1,
        "too_many": 1}
    assert report["media"]["exts"] == {".mp4": 1} and report["type"]["examples"] == [str(folder / "daten.xyz")]
    assert indexer.store.folders()[0]["unsearched"] == 9


def test_file_types_can_be_switched_off_and_added(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    folder.mkdir()
    (folder / "notiz.txt").write_text("eine Notiz", encoding="utf-8")
    (folder / "daten.json").write_text('{"a": 1}', encoding="utf-8")
    (folder / "spiel.xyz").write_text("Spielstand: Level 3", encoding="utf-8")
    fid = indexer.store.add_folder(str(folder))
    indexer.settings.update(types_off=[".json"], types_added=[".xyz"])

    indexer._scan_folder(fid)
    found = indexer.store.file_signatures(fid)
    assert set(found) == {str(folder / "notiz.txt"), str(folder / "spiel.xyz")}
    assert found[str(folder / "spiel.xyz")][3] == "indexed"  # read as plain text
    assert indexer.store.details(fid)["report"]["off"]["exts"] == {".json": 1}


def test_hidden_files_and_program_folders_when_wanted(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    (folder / "build").mkdir(parents=True)
    (folder / "build" / "notes.md").write_text("build notes", encoding="utf-8")
    (folder / ".notes.md").write_text("dot notes", encoding="utf-8")
    fid = indexer.store.add_folder(str(folder))
    indexer._scan_folder(fid)
    assert not indexer.store.file_signatures(fid)
    indexer.settings.update(hidden_files=True, program_folders=True)
    indexer._scan_folder(fid)
    assert set(indexer.store.file_signatures(fid)) == {str(folder / "build" / "notes.md"), str(folder / ".notes.md")}


def test_emails_and_their_attachments_are_both_searched(make_indexer, tmp_path):
    import samples

    indexer = make_indexer()
    folder = tmp_path / "mail"
    folder.mkdir()
    msg = folder / "Angebot.msg"
    msg.write_bytes(samples.outlook_msg("Angebot Dach", "Siehe Anhang", attachments=[("preise.txt", b"Ziegel " * 700)]))
    fid = indexer.store.add_folder(str(folder))

    indexer._scan_folder(fid)
    found = indexer.store.file_signatures(fid)
    assert {p: v[3] for p, v in found.items()} == {str(msg): "indexed", str(msg) + "\\preise.txt": "indexed"}
    indexer.settings.update(attachments=False)
    indexer._scan_folder(fid)
    assert set(indexer.store.file_signatures(fid)) == {str(msg)}


def test_unchanged_archives_are_not_opened_again(make_indexer, tmp_path, monkeypatch):
    import os
    import zipfile

    from quiekel_embed import archive

    indexer = make_indexer()
    folder = tmp_path / "docs"
    folder.mkdir()
    zpath = folder / "fotos.zip"
    with zipfile.ZipFile(zpath, "w") as z:
        z.writestr("a.txt", "Strand")
        z.writestr("b.exe", "kein Dokument")
    fid = indexer.store.add_folder(str(folder))
    opened = []
    walk = archive.walk
    monkeypatch.setattr(archive, "walk", lambda path, *a, **k: (opened.append(path), walk(path, *a, **k))[1])

    indexer._scan_folder(fid)
    indexer._scan_folder(fid)
    assert opened == [str(zpath)]  # the second scan knew what's inside
    assert set(indexer.store.file_signatures(fid)) == {str(zpath) + "\\a.txt"}
    assert indexer.store.details(fid)["report"]["program"]["n"] == 1  # (and why the rest isn't searched)
    st = zpath.stat()
    os.utime(zpath, (st.st_atime, st.st_mtime + 10))
    indexer._scan_folder(fid)
    assert opened == [str(zpath)] * 2
    indexer.settings.update(types_off=[".txt"])  # other types: looked into again
    indexer._scan_folder(fid)
    assert opened == [str(zpath)] * 3 and not indexer.store.file_signatures(fid)
