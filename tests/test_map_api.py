import numpy as np

from quiekel_embed import config


def save(store, folder_id, path, kind, texts):
    entry = {"folder_id": folder_id, "path": path, "size": 7, "mtime": 5.5, "kind": kind,
             "status": "indexed", "chunks": len(texts), "error": None, "search_name": path.rsplit("\\", 1)[-1]}
    rows = [{"path": path, "folder_id": folder_id, "chunk": i, "kind": kind, "text": t,
             "vector": np.full(config.EMBED_DIM, i + 1, np.float32).tolist()} for i, t in enumerate(texts)]
    store.save_files([entry], rows)


def test_the_map_card_gets_a_fingerprint_and_a_preview(client):
    store = client.backend.store
    folder = store.add_folder(r"C:\data")
    save(store, folder, r"C:\data\notes.txt", "text", ["first passage", "second passage"])
    save(store, folder, r"C:\data\scan.PDF", "document", ["page one"])
    save(store, folder, r"C:\data\IMG_1.jpg", "image", [""])
    ids = {p.rsplit("\\", 1)[-1]: sig[0] for p, sig in store.file_signatures(folder).items()}

    notes = client.get(f"/api/map/vector/{ids['notes.txt']}").json()
    assert len(notes["vector"]) == config.EMBED_DIM
    assert abs(np.linalg.norm(notes["vector"]) - 1) < 1e-3  # the mean of its passages, normalized
    assert notes["file"] == {"mtime": 5.5, "size": 7, "kind": "text", "preview": False, "passage": "first passage"}

    assert client.get(f"/api/map/vector/{ids['scan.PDF']}").json()["file"]["preview"] is True
    photo = client.get(f"/api/map/vector/{ids['IMG_1.jpg']}").json()["file"]
    assert photo["preview"] is True and photo["passage"] == ""
    assert client.get("/api/map/vector/999").status_code == 404
