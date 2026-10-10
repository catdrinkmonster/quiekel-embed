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

    # Switched off in Settings: the next scan forgets what's inside archives.
    indexer.settings.update(search_zips=False)
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
