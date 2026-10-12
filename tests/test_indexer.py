import threading
import time
from types import SimpleNamespace

import numpy as np
import pytest

from quiekel_embed import config
from quiekel_embed import indexer as indexer_module
from quiekel_embed.embedder import BadInput, EngineError, GpuBusy, ModelAsleep, NotEnoughMemory


class FakeEmbedder:
    """Stands in for the model: every text and image becomes the same unit vector."""

    def __init__(self, on_gpu=False):
        self.ready, self.on_gpu, self.setup = True, on_gpu, None
        self.pace, self.batch_scale, self.cpu_because = None, 1.0, "" if on_gpu else "none"
        self.last_used, self.unloaded, self.loads, self.preferred = time.monotonic(), [], 0, []
        self.status, self.error, self.engine, self.gpu_trouble = "ready", None, "directml" if on_gpu else "cpu", False
        self.download_gb, self.could_use_gpu, self.pid, self.keep_loaded = 0.9, on_gpu, None, True

    def memory_mb(self):
        return 0.0

    def unload(self, why):
        self.unloaded.append(why)
        self.ready, self.status = False, "asleep"

    def load(self, prefer_gpu=True, spare_gb=0.5):
        self.loads += 1
        self.preferred.append(prefer_gpu)
        self.ready, self.status = True, "ready"
        self.on_gpu = prefer_gpu and self.could_use_gpu

    def avoid_gpu(self, why):
        self.gpu_trouble = True
        self.unload(why)

    def documents(self, texts):
        return np.ones((len(texts), config.EMBED_DIM), np.float32) / config.EMBED_DIM ** 0.5

    def images(self, images):
        return np.ones((len(images), config.EMBED_DIM), np.float32) / config.EMBED_DIM ** 0.5


class FakeGovernor:
    def __init__(self):
        self.decision = SimpleNamespace(duty=1.0, level="full", code="full", params={})
        self.metrics = SimpleNamespace(ram_free_gb=16.0, vram_free_gb=None, game=False)

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


def test_waiting_for_video_memory_hands_the_gpu_back(make_indexer, tmp_path):
    indexer = make_indexer(FakeEmbedder(on_gpu=True))
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = indexer.store.add_folder(str(folder))
    gov = indexer.governor
    gov.decision.duty, gov.metrics.vram_free_gb = 0.0, 0.2  # a game took the video memory: everything waits

    t = in_thread(lambda: indexer._index_files(fid, str(folder), files))
    for _ in range(50):
        if indexer.embedder.unloaded:
            break
        time.sleep(0.1)
    assert indexer.embedder.unloaded == ["GPU memory almost full"], "the model kept its video memory although the game needed it"

    gov.decision.duty, gov.metrics.vram_free_gb = 1.0, 4.0  # the memory is back
    t.join(10)
    assert not t.is_alive()
    assert indexer.embedder.loads == 1  # (started again for the files)
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


def test_the_watcher_notes_changes_at_once_while_indexing_waits(make_indexer, tmp_path):
    import os

    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 3)
    fid = indexer.store.add_folder(str(folder))
    indexer._scan_folder(fid)
    indexer.pause()  # (or busy with a big folder: the watcher doesn't wait for it)
    new = folder / "einkaufsliste.txt"
    new.write_text("Milch, Eier, Mehl", encoding="utf-8")
    os.remove(files[1][0])

    indexer.enqueue_paths([str(new), files[1][0]])
    known = indexer.store.file_signatures(fid)
    assert known[str(new)][3] == "pending" and files[1][0] not in known  # right away
    assert [h["file_id"] for h in indexer.store.keyword_search('"einkaufsliste"*', 10)] == [known[str(new)][0]]
    assert indexer.store.folders()[0]["pending"] == 1

    indexer.resume()
    while not indexer.jobs.empty():  # what the indexer does in turn
        indexer._do(*indexer.jobs.get())
    assert indexer.store.file_signatures(fid)[str(new)][3] == "indexed"
    assert indexer.store.folders()[0]["pending"] == 0


def test_a_file_back_before_its_turn_stays(make_indexer, tmp_path):
    import os

    indexer = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 2)
    fid = indexer.store.add_folder(str(folder))
    indexer._scan_folder(fid)
    path = files[1][0]
    os.rename(path, path + ".bak")
    indexer.enqueue_paths([path])  # deleted...
    os.rename(path + ".bak", path)
    indexer.enqueue_paths([path])  # ...and back, before the indexer got to either
    while not indexer.jobs.empty():
        indexer._do(*indexer.jobs.get())
    assert indexer.store.file_signatures(fid)[path][3] == "indexed"


def test_a_phone_copy_leaves_out_the_apps_own_data(make_indexer, tmp_path):
    from quiekel_embed.filetypes import Report

    indexer = make_indexer()
    phone = tmp_path / "Phone copy"
    cache = phone / "Android" / "data" / "com.example.music" / "cache"
    cache.mkdir(parents=True)
    (cache / "cover").write_text("not for searching", encoding="utf-8")
    saved = phone / "Android" / "media" / "com.example.chat"
    saved.mkdir(parents=True)
    (saved / "note.txt").write_text("saved from a chat", encoding="utf-8")
    report = Report()
    assert [p for p, *_ in indexer._walk(str(phone), 0, report)] == [str(saved / "note.txt")]
    assert report.data["program_folder"]["n"] == 1


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


def test_light_lets_the_model_go_when_unused_and_wakes_it_for_work(make_indexer):
    e = FakeEmbedder()
    ix = make_indexer(e)
    e.last_used = ix._last_work = time.monotonic() - 3600  # nothing for an hour
    ix._manage_device(working=False)
    assert e.unloaded == []  # Balanced (the default) keeps it ready
    ix.settings.update(perf_mode="gentle")  # Light
    e.last_used = time.monotonic()  # just searched
    ix._manage_device(working=False)
    assert e.unloaded == []
    e.last_used = time.monotonic() - 3600
    ix._manage_device(working=False)
    assert e.unloaded and not e.ready
    ix._ensure_model()  # a new file to index
    assert e.ready and e.loads == 1


def test_the_model_leaves_the_graphics_card_when_video_memory_runs_out(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    ix.governor.metrics.vram_free_gb = 0.2
    ix._manage_device(working=False)
    assert e.unloaded == ["GPU memory almost full"]  # (all of it back; it starts again where it fits)


def test_a_smaller_pc_lets_the_model_go_when_its_not_used(make_indexer):
    e = FakeEmbedder()
    e.keep_loaded = False  # (the light setup, or built-in graphics: the PC's own memory)
    ix = make_indexer(e)
    e.last_used = ix._last_work = time.monotonic() - 3600  # nothing for an hour
    ix.settings.update(perf_mode="full")  # Maximum keeps it
    ix._manage_device(working=False)
    assert e.unloaded == []
    ix.settings.update(perf_mode="balanced")  # Balanced too, on a bigger PC; on a smaller one not
    ix._manage_device(working=False)
    assert e.unloaded == ["not used for a while"]


def test_balanced_keeps_the_model_ready_on_the_graphics_card(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    e.last_used = ix._last_work = time.monotonic() - 3600  # nothing for an hour
    ix.governor.metrics.vram_free_gb = 4.0
    for mode in ("full", "balanced"):
        ix.settings.update(perf_mode=mode)
        ix._manage_device(working=False)
        assert e.ready and not e.unloaded, mode  # (ready for the next search)
    ix.settings.update(perf_mode="gentle")  # Light lets it go
    ix._manage_device(working=False)
    assert e.unloaded == ["not used for a while"]


def test_balanced_gives_the_card_back_when_another_app_needs_its_memory(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    ix.governor.metrics.vram_free_gb = 0.8
    ix._manage_device(working=False)
    assert e.unloaded == []  # (just used: it waits a while)
    e.last_used = ix._last_work = time.monotonic() - 3600
    ix.settings.update(perf_mode="full")  # Maximum keeps it
    ix._manage_device(working=False)
    assert e.unloaded == []
    ix.settings.update(perf_mode="balanced")
    ix._manage_device(working=False)
    assert e.unloaded == ["another app needs the graphics card's memory"]


def test_a_game_gets_the_graphics_card_back_and_the_model_waits_for_it(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    ix.governor.metrics.game = True
    ix._manage_device(working=True)  # even in the middle of a folder
    assert e.unloaded == ["a game is running"]

    e.ready, e.status = False, "asleep"  # (Light: the model had gone to sleep)
    t = in_thread(ix._ensure_model)  # new files arrive during the game
    time.sleep(0.3)
    assert e.loads == 0
    ix.governor.metrics.game = False
    t.join(5)
    assert e.loads == 1

    e.on_gpu, e.unloaded = True, []
    ix.settings.update(perf_mode="full")  # Maximum: anytime
    ix.governor.metrics.game = True
    ix._manage_device(working=True)
    assert e.unloaded == []


def test_a_game_at_start_keeps_the_model_asleep_until_it_ends(make_indexer):
    e = FakeEmbedder()
    e.ready, e.status = False, "not loaded"
    ix = make_indexer(e)
    ix.governor.metrics.game = True
    t = in_thread(ix._rest_at_start)
    time.sleep(0.3)
    assert e.status == "asleep" and e.loads == 0  # (a search would wake it: wake_model)
    ix.governor.metrics.game = False
    t.join(5)
    assert not t.is_alive()


def test_off_stops_everything_and_turning_it_on_looks_again(make_indexer, tmp_path):
    e = FakeEmbedder()
    ix = make_indexer(e)
    folder = tmp_path / "docs"
    files = notes(folder, 3)
    fid = ix.store.add_folder(str(folder))
    ix.watcher.sync()
    assert list(ix.watcher.watches) == [fid]

    ix.enqueue_scan(fid)
    ix.settings.update(perf_mode="off")
    ix.set_off(True)
    assert ix.jobs.empty() and ix.watcher.watches == {}  # nothing queued, nothing watched
    ix.enqueue_paths([files[0][0]])
    assert ix.jobs.empty()
    ix._index_files(fid, str(folder), files)  # (work in hand stops at once)
    assert ix.store.file_signatures(fid) == {}
    ix._manage_device(working=False)
    assert e.unloaded == ["switched off"]  # the model leaves; a search wakes it for a while

    ix.settings.update(perf_mode="balanced")
    ix.set_off(False)
    assert list(ix.watcher.watches) == [fid] and [job for job, _ in list(ix.jobs.queue)] == ["scan"]


def test_off_lets_the_model_go_right_after_the_pc_starts(make_indexer, monkeypatch):
    e = FakeEmbedder()
    ix = make_indexer(e)
    ix.settings.update(perf_mode="off")
    monkeypatch.setattr(time, "monotonic", lambda: 60.0)  # a minute after boot, no search yet
    ix._manage_device(working=False)
    assert e.unloaded == ["switched off"]


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
    (folder / "daten.xyz").write_bytes(b"x\x00" * 10)  # (not text: a type the app doesn't know stays unread)
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


def test_types_the_app_doesnt_know_are_read_when_theres_text_inside(make_indexer, tmp_path):
    indexer = make_indexer()
    folder = tmp_path / "docs"
    folder.mkdir()
    (folder / "notiz.txt").write_text("eine Notiz", encoding="utf-8")
    (folder / "daten.json").write_text('{"a": 1}', encoding="utf-8")
    for i in range(4):
        (folder / f"spiel{i}.xyz").write_text(f"Spielstand: Level {i}, Zauberstab gefunden, Kröte besiegt", encoding="cp1252")
    (folder / "modell.qqq").write_bytes(bytes(range(256)) * 8)
    fid = indexer.store.add_folder(str(folder))
    indexer.settings.update(types_off=[".json"])

    indexer._scan_folder(fid)
    found = indexer.store.file_signatures(fid)
    assert set(found) == {str(folder / "notiz.txt"), *(str(folder / f"spiel{i}.xyz") for i in range(4))}
    assert found[str(folder / "spiel3.xyz")][3] == "indexed"  # read as plain text
    report = indexer.store.details(fid)["report"]
    assert report["off"]["exts"] == {".json": 1} and report["type"]["exts"] == {".qqq": 1}
    assert indexer.settings.get("types_found") == {".xyz": "text", ".qqq": "binary"}  # kept for File types

    indexer.settings.update(types_off=[".json", ".xyz"])  # switched off there
    indexer._scan_folder(fid)
    assert set(indexer.store.file_signatures(fid)) == {str(folder / "notiz.txt")}


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


# ---- weak PCs: an engine in trouble, and memory --------------------------------------------


class FlakyEmbedder(FakeEmbedder):
    """Fails the first times it's asked, the ways an engine in trouble does."""

    def __init__(self, failures):
        super().__init__()
        self.failures = list(failures)

    def documents(self, texts):
        if self.failures:
            raise self.failures.pop(0)
        return super().documents(texts)


def statuses(ix, fid) -> dict[str, str]:
    return {p.rsplit("\\", 1)[-1].rsplit("/", 1)[-1]: v[3] for p, v in ix.store.file_signatures(fid).items()}


def no_waiting(monkeypatch):
    """The indexer's waits between tries (10 seconds and more) take no time; its short ones stay."""
    monkeypatch.setattr(indexer_module, "time", SimpleNamespace(
        sleep=lambda s: time.sleep(s) if s <= 1 else None, monotonic=time.monotonic,
        perf_counter=time.perf_counter, time=time.time))


def test_an_engine_in_trouble_never_marks_files_unreadable(make_indexer, tmp_path, monkeypatch):
    no_waiting(monkeypatch)
    e = FlakyEmbedder([EngineError("DirectML failed"), GpuBusy("out of memory"), ModelAsleep("let go")])
    ix = make_indexer(e)
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = ix.store.add_folder(str(folder))
    ix._index_files(fid, str(folder), files)
    assert set(statuses(ix, fid).values()) == {"indexed", "skipped"} and not e.failures


def test_a_file_the_model_cant_take_is_the_only_one_marked(make_indexer, tmp_path):
    class Picky(FakeEmbedder):
        def documents(self, texts):
            if any("number 3 " in t for t in texts):
                raise BadInput("ValueError: broken")
            return super().documents(texts)

    ix = make_indexer(Picky())
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = ix.store.add_folder(str(folder))
    ix._index_files(fid, str(folder), files)
    assert [name for name, status in statuses(ix, fid).items() if status == "error"] == ["note_03.txt"]


def test_memory_almost_full_for_a_while_lets_the_model_go(make_indexer):
    e = FakeEmbedder()
    ix = make_indexer(e)
    ix.settings.update(perf_mode="full")  # whatever the setting
    ix.governor.decision.code = "ram_full"
    ix._manage_device(working=True)
    assert e.unloaded == []  # not at once: memory may free up again
    ix._ram_low_since -= indexer_module.RAM_RELEASE_AFTER_S
    ix._manage_device(working=True)
    assert e.unloaded == ["memory almost full"]


def test_the_model_waits_for_memory_before_it_loads_and_says_so(make_indexer, monkeypatch):
    class Tight(FakeEmbedder):
        room = False

        def load(self, prefer_gpu=True, spare_gb=0.5):
            self.spare = spare_gb
            if not self.room:
                raise NotEnoughMemory(0.6, 1.4)
            super().load(prefer_gpu, spare_gb)

    e = Tight()
    e.ready, e.status = False, "asleep"
    ix = make_indexer(e)
    notes_seen = []

    def sleep(seconds):
        if seconds < 5:  # (the watcher's naps)
            return time.sleep(seconds)
        notes_seen.append(ix.progress().get("note"))
        e.room = True  # other apps closed meanwhile

    monkeypatch.setattr(indexer_module, "time", SimpleNamespace(
        sleep=sleep, monotonic=time.monotonic, perf_counter=time.perf_counter, time=time.time))
    ix._ensure_model()
    assert e.ready and e.spare == indexer_module.INDEX_SPARE_GB  # (more room than a search needs)
    assert notes_seen == [{"key": "note.waiting_ram", "params": {"gb": 0.6}}]
    assert ix.progress().get("note") is None


def test_light_loads_the_model_only_when_theres_something_for_it(make_indexer):
    e = FakeEmbedder()
    e.ready, e.status = False, "not loaded"
    ix = make_indexer(e)
    ix.settings.update(perf_mode="gentle")
    ix._load_at_start()
    assert (e.loads, e.status) == (0, "asleep")
    ix.settings.update(perf_mode="balanced")
    ix._load_at_start()
    assert (e.loads, e.ready) == (1, True)


# ---- pausing and removing folders ----------------------------------------------------------


def test_removing_the_folder_being_indexed_stops_it_even_while_everything_is_paused(make_indexer, tmp_path):
    ix = make_indexer()
    folder = tmp_path / "docs"
    files = notes(folder, 10)
    fid = ix.store.add_folder(str(folder))
    ix.pause()
    t = in_thread(lambda: ix._index_files(fid, str(folder), files))
    time.sleep(0.3)
    assert t.is_alive()  # waiting, paused
    ix.remove_folder(fid)
    t.join(5)
    assert not t.is_alive()  # gone at once, not when everything goes on again


def test_a_folder_paused_on_its_card_waits_and_the_others_go_on(make_indexer, tmp_path):
    ix = make_indexer()
    a, b = tmp_path / "a", tmp_path / "b"
    files_a, files_b = notes(a, 10), notes(b, 10)
    fa, fb = ix.store.add_folder(str(a)), ix.store.add_folder(str(b))
    ix.pause_folder(fa)
    ix._scan_folder(fa)  # (held: nothing happens)
    ix._index_files(fb, str(b), files_b)
    assert ix.store.file_signatures(fa) == {}
    assert set(statuses(ix, fb).values()) == {"indexed", "skipped"}
    assert ix.paused_folders == {fa}
    ix.resume_folder(fa)
    assert ix.paused_folders == set() and [job for job, _ in list(ix.jobs.queue)] == ["scan"]
    ix._scan_folder(fa)
    assert set(statuses(ix, fa).values()) == {"indexed", "skipped"} and len(files_a) == 10


def test_try_again_loads_the_model_even_when_nobody_waits_for_it(make_indexer):
    e = FakeEmbedder()
    e.ready, e.status = False, "error"  # (it failed for a search)
    ix = make_indexer(e)
    ix.retry_model()
    deadline = time.monotonic() + 5
    while not e.ready and time.monotonic() < deadline:
        time.sleep(0.05)
    assert e.ready and e.loads == 1


def test_a_search_during_a_game_starts_the_model_on_the_processor(make_indexer):
    e = FakeEmbedder(on_gpu=True)
    e.ready, e.status = False, "asleep"
    ix = make_indexer(e)
    ix.governor.metrics.game = True  # Balanced: the game keeps its graphics card
    ix.wake_model()
    for _ in range(50):
        if e.ready:
            break
        time.sleep(0.05)
    assert e.preferred == [False] and not e.on_gpu


def test_after_a_game_the_work_moves_back_to_the_graphics_card(make_indexer, tmp_path):
    e = FakeEmbedder()
    e.could_use_gpu = True  # (started on the processor during the game, for a search)
    ix = make_indexer(e)
    folder = tmp_path / "docs"
    fid = ix.store.add_folder(str(folder))
    ix._index_files(fid, str(folder), notes(folder, 3))  # the game is over: there's work
    assert e.unloaded == ["the graphics card is free again"] and e.preferred == [True] and e.on_gpu
    assert len(ix.store.file_signatures(fid)) == 3


def test_after_the_graphics_card_ran_full_the_processor_works_for_a_while(make_indexer, tmp_path):
    e = FakeEmbedder(on_gpu=True)
    ix = make_indexer(e)
    ix.governor.metrics.vram_free_gb = 0.2  # (the model's own memory, or another app's)
    ix._manage_device(working=True)
    assert e.unloaded == ["GPU memory almost full"]
    ix.governor.metrics.vram_free_gb = 3.0  # free again once the model is gone
    folder = tmp_path / "docs"
    files = notes(folder, 7)
    fid = ix.store.add_folder(str(folder))
    ix._index_files(fid, str(folder), files[:3])
    assert e.preferred == [False] and len(ix.store.file_signatures(fid)) == 3  # (not straight back)
    ix._vram_full_at -= indexer_module.BACK_TO_GPU_S
    ix._moved_to_gpu -= indexer_module.BACK_TO_GPU_S
    ix._index_files(fid, str(folder), files[3:])
    assert e.preferred == [False, True] and e.on_gpu  # a while later, back to the card
    assert len(ix.store.file_signatures(fid)) == 7


def test_the_model_stays_on_the_processor_when_the_card_cant_take_it(make_indexer, tmp_path):
    e = FakeEmbedder()  # no graphics card, or it failed this session
    ix = make_indexer(e)
    folder = tmp_path / "docs"
    fid = ix.store.add_folder(str(folder))
    ix._index_files(fid, str(folder), notes(folder, 3))
    assert e.unloaded == [] and e.loads == 0
