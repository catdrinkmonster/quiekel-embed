import numpy as np
import pytest

from quiekel_embed import config
from quiekel_embed.filemap import FileMap, align, place, sample


@pytest.fixture
def store(tmp_path, monkeypatch):
    for name, value in (("DATA_DIR", tmp_path), ("DB_PATH", tmp_path / "state.db"), ("VECTORS_DIR", tmp_path / "vectors")):
        monkeypatch.setattr(config, name, value)
    from quiekel_embed.store import Store

    return Store()


def add_files(store, folder, start, n, centers, rng):
    entries, rows = [], []
    for i in range(start, start + n):
        group = i % len(centers)
        v = centers[group] + 0.6 * rng.standard_normal(config.EMBED_DIM) / np.sqrt(config.EMBED_DIM)
        v = (v / np.linalg.norm(v)).astype(np.float32)
        path = f"C:\\m\\{group}\\f{i}.md"
        entries.append({"folder_id": folder, "path": path, "size": 10, "mtime": 1.0, "kind": "text",
                        "status": "indexed", "chunks": 1, "error": None, "search_name": f"f{i}"})
        rows.append({"path": path, "folder_id": folder, "chunk": 0, "kind": "text", "text": "x", "vector": v.tolist()})
    store.save_files(entries, rows)


def coords(m: FileMap) -> dict:
    return {p["id"]: np.array([p["x"], p["y"], p["z"]]) for p in m.get()["points"]}


def refresh(m: FileMap):
    m._compute(m.store.version)  # what the background thread does


def unit_centers(rng, n):
    c = rng.standard_normal((n, config.EMBED_DIM))
    return c / np.linalg.norm(c, axis=1, keepdims=True)


def test_the_map_keeps_its_layout_places_new_files_and_survives_a_restart(store):
    rng = np.random.default_rng(1)
    centers = unit_centers(rng, 4)
    folder = store.add_folder(r"C:\m")
    add_files(store, folder, 0, 200, centers, rng)
    m = FileMap(store)
    m._computed_at = 1e18  # (no background thread in this test)
    assert m.get()["status"] == "computing"
    refresh(m)
    first = m.get()
    assert first["status"] == "ready" and len(first["points"]) == 200
    before = coords(m)

    add_files(store, folder, 200, 8, centers, rng)  # a few more, like the ones there
    assert m.get()["status"] == "updating"
    refresh(m)
    after = coords(m)
    assert len(after) == 208 and m.get()["rev"] == first["rev"] + 1
    assert all(np.allclose(before[i], after[i]) for i in before)  # nobody else moved
    paths = {f.id: f.path for f in store.files_by_ids(list(after)).values()}
    group = {i: paths[i].split("\\")[2] for i in after}
    for i in set(after) - set(before):  # each new file lands among its own group
        mine = np.array([after[j] for j in before if group[j] == group[i]])
        rest = np.array([after[j] for j in before if group[j] != group[i]])
        near = np.sort(np.linalg.norm(mine - after[i], axis=1))[:5].mean()
        far = np.sort(np.linalg.norm(rest - after[i], axis=1))[:5].mean()
        assert near < far

    again = FileMap(store)  # the app restarts
    again._computed_at = 1e18
    shown = again.get()
    assert shown["status"] == "updating" and len(shown["points"]) == 208  # right away, from disk
    assert all(np.allclose(after[i], c, atol=1e-4) for i, c in coords(again).items())


def test_much_new_lays_out_again_but_keeps_the_picture(store):
    rng = np.random.default_rng(2)
    centers = unit_centers(rng, 3)
    folder = store.add_folder(r"C:\m")
    add_files(store, folder, 0, 160, centers, rng)
    m = FileMap(store)
    m._computed_at = 1e18
    refresh(m)
    before = coords(m)
    add_files(store, folder, 160, 90, centers, rng)  # more than 30% new: laid out again
    refresh(m)
    after = coords(m)
    assert len(after) == 250
    # The groups stay where they were (not flipped or spun around), even if dots inside a group
    # are shuffled.
    paths = {f.id: f.path for f in store.files_by_ids(list(before)).values()}
    for g in "012":
        ids = [i for i in before if paths[i].split("\\")[2] == g]
        moved = np.linalg.norm(np.mean([before[i] for i in ids], axis=0) - np.mean([after[i] for i in ids], axis=0))
        assert moved < 0.3


def test_a_stable_sample_of_big_indexes():
    a = sample(np.arange(1, 10_001), 1000)
    b = sample(np.arange(1, 10_201), 1000)  # 200 files more
    assert len(a) == len(b) == 1000
    assert len(np.intersect1d(a, b)) > 950  # nearly the same files stay on the map


def test_align_and_place():
    rng = np.random.default_rng(0)
    target = rng.uniform(-1, 1, (50, 3)).astype(np.float32)
    turned = target @ np.array([[0, -1, 0], [1, 0, 0], [0, 0, -1]], np.float32) * 0.5
    assert np.allclose(align(turned, target), target, atol=1e-4)
    vecs = np.eye(4, config.EMBED_DIM, dtype=np.float32)
    spots = np.array([[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0]], np.float32)
    new = vecs[2:3] * 0.9 + vecs[0:1] * 0.1
    pos = place(new / np.linalg.norm(new), vecs, spots, np.array([7]))
    assert np.linalg.norm(pos[0] - spots[2]) < 0.1
