import numpy as np
import pytest

from quiekel_embed import config
from quiekel_embed.search import fts_query, fuse, has_all_terms, query_terms


def test_query_terms_drop_stopwords_and_accents():
    assert query_terms("Photo of a Whiteboard") == ["photo", "whiteboard"]
    assert query_terms("Rechnung für Strom") == ["rechnung", "strom"]
    assert query_terms("Électricité") == ["electricite"]
    assert query_terms("the") == ["the"]  # only stopwords: keep them


def test_fts_query_prefixes_last_term():
    assert fts_query(["invoice", "2026", "114"]) == '"invoice" OR "2026" OR "114"*'
    assert fts_query([]) is None


def test_has_all_terms_matches_word_prefixes():
    assert has_all_terms(["img", "0042"], "IMG_0042.jpg photos", "")
    assert not has_all_terms(["img", "0043"], "IMG_0042.jpg", "")
    assert has_all_terms(["invoice"], "invoices 2026", "")  # longer words: the start of a word counts
    assert not has_all_terms(["cat"], "catalog.py", "")  # short words: whole words only
    assert has_all_terms(["cat"], "cat_food.md", "")


def vec(fid, chunk, dist, text="t"):
    return {"file_id": fid, "chunk": chunk, "_distance": dist, "text": text}


def kw(fid, chunk, name="", text=""):
    return {"file_id": fid, "chunk": chunk, "name": name, "text": text, "rank": -1.0}


def test_exact_keyword_match_beats_semantic_neighbours():
    vectors = [vec(1, 0, 0.30), vec(2, 0, 0.35), vec(3, 0, 0.40)]
    keywords = [kw(3, 0, name="IMG_0042.jpg photos")]
    out = fuse(vectors, keywords, "IMG_0042", limit=10)
    assert out[0]["file_id"] == 3 and out[0]["exact"] and out[0]["keyword"]


def test_keyword_only_results_work_before_the_model_loads():
    out = fuse([], [kw(7, 2, text="Invoice 2026-114")], "invoice 2026-114", limit=10)
    assert [r["file_id"] for r in out] == [7]
    assert out[0]["similarity"] is None and out[0]["chunk"] == 2


def test_all_your_words_first_then_by_meaning():
    vectors = [vec(1, 0, 0.20), vec(2, 0, 0.30), vec(3, 0, 0.25)]
    out = fuse(vectors, [kw(2, 0, text="red apple")], "red apple", limit=10)
    assert [r["file_id"] for r in out] == [2, 1, 3]  # 2 has all the words; then 0.80 > 0.75


def test_keyword_finds_get_their_meaning_score():
    out = fuse([vec(1, 0, 0.30)], [kw(9, 0, text="something about pears")], "pears apples", 10,
               similarity_of=lambda ids: {9: 0.9})
    assert [r["file_id"] for r in out] == [9, 1]  # neither has all the words: 0.9 beats 0.7
    assert out[0]["similarity"] == 0.9


def test_snippet_switches_to_the_passage_with_the_words():
    out = fuse([vec(5, 0, 0.2, "intro")], [kw(5, 4, text="the landlord letter")], "landlord letter", 5)
    assert out[0]["snippet"] == "the landlord letter" and out[0]["chunk"] == 4


# ---- keyword index in the store ------------------------------------------


@pytest.fixture
def store(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "DATA_DIR", tmp_path)
    monkeypatch.setattr(config, "DB_PATH", tmp_path / "state.db")
    monkeypatch.setattr(config, "VECTORS_DIR", tmp_path / "vectors")
    from quiekel_embed.store import Store

    return Store()


def save(store, folder_id, path, kind, texts, name):
    entry = {"folder_id": folder_id, "path": path, "size": 1, "mtime": 1.0, "kind": kind,
             "status": "indexed", "chunks": len(texts), "error": None, "search_name": name}
    rows = [{"path": path, "folder_id": folder_id, "chunk": i, "kind": kind, "text": t,
             "vector": np.ones(config.EMBED_DIM, np.float32).tolist()} for i, t in enumerate(texts)]
    store.save_files([entry], rows)


def test_store_keyword_index_follows_adds_updates_and_removals(store):
    fid = store.add_folder(r"C:\data")
    save(store, fid, r"C:\data\a.txt", "text", ["electricity bill", "second part"], "a.txt")
    save(store, fid, r"C:\data\IMG_0042.jpg", "image", [""], "IMG_0042.jpg photos")

    assert [h["chunk"] for h in store.keyword_search('"electricity"', 10)] == [0]
    assert store.keyword_search('"0042"', 10, kinds=("image",))[0]["name"].startswith("IMG_0042")
    assert store.keyword_search('"0042"', 10, kinds=("text",)) == []

    save(store, fid, r"C:\data\a.txt", "text", ["water bill"], "a.txt")  # file changed
    assert store.keyword_search('"electricity"', 10) == []
    assert len(store.keyword_search('"water"', 10)) == 1

    store.remove_paths([r"C:\data\a.txt"])
    assert store.keyword_search('"water"', 10) == []
    store.remove_folder(fid)
    assert store.keyword_search('"0042"', 10) == []
    assert store.vector_count() == 0


def test_searching_inside_one_folder(store):
    fid = store.add_folder(r"C:\data")
    save(store, fid, r"C:\data\a\x.txt", "text", ["apple pie"], "x.txt a")
    save(store, fid, r"C:\data\a\b\y.txt", "text", ["apple juice"], "y.txt a b")
    save(store, fid, r"C:\data\ab\w.txt", "text", ["apple cake"], "w.txt ab")
    save(store, fid, r"C:\data\c\z.txt", "text", ["apple tree"], "z.txt c")

    assert len(store.file_ids_under(r"C:\data\a")) == 2  # not the files in "ab"
    assert [(d["name"], d["count"]) for d in store.subdirs(r"C:\data")] == [("a", 2), ("ab", 1), ("c", 1)]
    assert [d["name"] for d in store.subdirs(r"C:\data\a")] == ["b"]
    assert len(store.keyword_search('"apple"', 10, under=r"C:\DATA\a\\")) == 2  # case and trailing \ don't matter

    sims = store.similarities(np.ones(config.EMBED_DIM, np.float32), store.file_ids_under(r"C:\data"))
    assert len(sims) == 4 and all(abs(s - 1) < 1e-5 for s in sims.values())


def test_settings_persist(store):
    from quiekel_embed.store import Settings

    s = Settings(store)
    assert s.get("perf_mode") == "balanced"
    s.update(perf_mode="gentle", free_gpu_idle=False)
    again = Settings(store)
    assert again.get("perf_mode") == "gentle" and again.get("free_gpu_idle") is False


def save_vectors(store, folder_id, path, kind, vectors):
    entry = {"folder_id": folder_id, "path": path, "size": 1, "mtime": 1.0, "kind": kind, "status": "indexed",
             "chunks": len(vectors), "error": None, "search_name": path.rsplit("\\", 1)[-1]}
    rows = [{"path": path, "folder_id": folder_id, "chunk": i, "kind": kind, "text": f"{path} #{i}",
             "vector": v.tolist()} for i, v in enumerate(vectors)]
    store.save_files([entry], rows)


def unit(rng, n):
    v = rng.standard_normal((n, config.EMBED_DIM)).astype(np.float32)
    return v / np.linalg.norm(v, axis=1, keepdims=True)


def test_search_in_memory_finds_the_same_as_the_table_on_disk(store):
    rng = np.random.default_rng(3)
    a, b = store.add_folder(r"C:\a"), store.add_folder(r"C:\b")
    for i in range(40):
        kind = "image" if i % 4 == 0 else "text"
        where = "a" if i % 2 else "b"
        save_vectors(store, a if i % 2 else b, f"C:\\{where}\\f{i}.x", kind, unit(rng, 1 if kind == "image" else 3))
    q = unit(rng, 1)[0]
    some = store.file_ids_under(r"C:\a")[:7]
    filters = [{}, {"kinds": ("image",)}, {"folder_id": a}, {"file_ids": some}, {"kinds": ("text",), "folder_id": b}]
    in_memory = [store.search(q, 12, **f) for f in filters]
    assert store.vectors.ready
    store.keep_vectors_in_memory(False)
    on_disk = [store.search(q, 12, **f) for f in filters]
    for mem, disk in zip(in_memory, on_disk):
        assert [(h["file_id"], h["chunk"], h["kind"], h["text"]) for h in mem] == \
               [(h["file_id"], h["chunk"], h["kind"], h["text"]) for h in disk]
        assert np.allclose([h["_distance"] for h in mem], [h["_distance"] for h in disk], atol=1e-5)
    assert all(h["kind"] == "image" for h in in_memory[1]) and len(in_memory[1]) == 10
    assert {h["file_id"] for h in in_memory[3]} <= set(some)
    assert store.search(q, 5, file_ids=[]) == []


def test_vectors_in_memory_follow_every_write(store):
    rng = np.random.default_rng(4)
    fid = store.add_folder(r"C:\a")
    for i in range(10):
        save_vectors(store, fid, f"C:\\a\\f{i}.x", "text", unit(rng, 2))
    store.search(unit(rng, 1)[0], 1)  # (fills them on first use)
    assert store.vectors.ready
    target = unit(rng, 1)
    save_vectors(store, fid, r"C:\a\f3.x", "text", target)  # f3 changed: now one passage, the target
    top = store.search(target[0], 3)
    f3 = store.signature(r"C:\a\f3.x")[0]
    assert top[0]["file_id"] == f3 and top[0]["_distance"] < 1e-5
    assert [h["chunk"] for h in top if h["file_id"] == f3] == [0]  # its old passages are gone
    assert len(store.file_vectors(f3)) == 1
    store.remove_paths([r"C:\a\f3.x"])
    assert all(h["file_id"] != f3 for h in store.search(target[0], 20))
    store.remove_folder(fid)
    assert store.search(target[0], 20) == [] and store.vectors.rows == 0


def test_searches_dont_wait_for_compaction(store, monkeypatch):
    import threading
    import time

    rng = np.random.default_rng(6)
    fid = store.add_folder(r"C:\a")
    for i in range(5):
        save_vectors(store, fid, f"C:\\a\\f{i}.x", "text", unit(rng, 2))
    q = unit(rng, 1)[0]
    store.search(q, 3)
    busy = threading.Event()

    def slow_compaction(**kwargs):
        busy.set()
        time.sleep(1.0)

    monkeypatch.setattr(store.table, "optimize", slow_compaction)
    t = threading.Thread(target=store.optimize)
    t.start()
    busy.wait(5)
    started = time.perf_counter()
    assert len(store.search(q, 3)) == 3
    assert time.perf_counter() - started < 0.5
    t.join()


def test_a_new_model_or_format_starts_the_index_over(store, monkeypatch):
    from quiekel_embed.store import Store

    rng = np.random.default_rng(7)
    fid = store.add_folder(r"C:\a")
    save_vectors(store, fid, r"C:\a\f.x", "text", unit(rng, 2))
    assert store.vector_count() == 2
    monkeypatch.setattr(config, "MODEL_ID", "someone/another-model")
    again = Store()  # the vector table opens later, on first use, and is dropped then
    assert again.vector_count() == 0 and again.signature(r"C:\a\f.x") is None
    assert again.folder(fid)  # the folders stay


def test_unreadable_files_warn_once(store):
    fid = store.add_folder(r"C:\a")

    def unreadable(path):
        store.save_files([{"folder_id": fid, "path": path, "size": 1, "mtime": 1.0, "kind": "image",
                           "status": "error", "chunks": 0, "error": "!damaged_image"}], [])

    unreadable(r"C:\a\x.jpg")
    unreadable(r"C:\a\y.jpg")
    f = store.folders()[0]
    assert (f["errors"], f["unreadable"]) == (2, 2)
    store.errors_seen(fid)  # looked at in the folder's details
    f = store.folders()[0]
    assert (f["errors"], f["unreadable"]) == (0, 2)
    assert [e["seen"] for e in store.errors(fid)] == [True, True]  # still listed
    unreadable(r"C:\a\x.jpg")  # tried again, still broken: no new warning
    unreadable(r"C:\a\z.jpg")  # a new one warns
    assert [e["path"] for e in store.errors(fid) if not e["seen"]] == [r"C:\a\z.jpg"]
    save_vectors(store, fid, r"C:\a\y.jpg", "image", unit(np.random.default_rng(1), 1))  # fixed
    unreadable(r"C:\a\y.jpg")  # broken again later: that's news, so it warns
    assert {e["path"] for e in store.errors(fid) if not e["seen"]} == {r"C:\a\y.jpg", r"C:\a\z.jpg"}
    assert store.folders()[0]["errors"] == 2


def test_new_files_dont_cost_a_delete_in_the_vector_table(store):
    rng = np.random.default_rng(5)
    fid = store.add_folder(r"C:\a")
    v0 = store.table.version
    save_vectors(store, fid, r"C:\a\new.x", "text", unit(rng, 2))
    assert store.table.version == v0 + 1  # just the added rows
    save_vectors(store, fid, r"C:\a\new.x", "text", unit(rng, 2))
    assert store.table.version == v0 + 3  # changed: old rows deleted, new ones added
    assert store.vector_count() == 2
