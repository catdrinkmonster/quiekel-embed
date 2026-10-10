H = {"x-quiekel-embed": "1"}


def settings(client):
    return client.get("/api/status").json()["settings"]


def test_defaults_ask_before_going_online(client):
    s = settings(client)
    assert s["check_updates"] is False  # update checks are opt-in
    assert s["search_zips"] is True
    assert (s["view_files"], s["view_images"]) == ("cards", "grid")


def test_result_views_are_remembered_per_kind(client):
    assert client.post("/api/settings", json={"view_files": "list"}, headers=H).status_code == 200
    assert client.post("/api/settings", json={"view_images": "cards"}, headers=H).status_code == 200
    s = settings(client)
    assert (s["view_files"], s["view_images"]) == ("list", "cards")
    r = client.post("/api/settings", json={"view_files": "carousel"}, headers=H)
    assert r.status_code == 400 and r.json()["detail"]["key"] == "err.unknown_view"


def test_a_bug_report_opens_githubs_form_with_only_the_version(client, monkeypatch):
    import webbrowser

    opened = []
    monkeypatch.setattr(webbrowser, "open", opened.append)
    assert client.post("/api/report-bug", headers=H).status_code == 200
    assert opened and opened[0].startswith("https://github.com/catdrinkmonster/quiekel-embed/issues/new?body=")
    assert "Quiekel+Embed" in opened[0] and "Users" not in opened[0]


def test_the_page_opens_in_the_saved_theme(client):
    assert 'data-theme-mode="auto"' in client.get("/").text
    client.post("/api/settings", json={"theme": "dark"}, headers=H)
    assert 'data-theme-mode="dark"' in client.get("/").text


def test_memory_setting_keeps_search_data_in_memory_or_not(client):
    store = client.backend.store
    assert settings(client)["memory"] == "fast"
    assert client.post("/api/settings", json={"memory": "lean"}, headers=H).status_code == 200
    assert settings(client)["memory"] == "lean" and not store.vectors.ready and not store._keep_vectors
    r = client.post("/api/settings", json={"memory": "huge"}, headers=H)
    assert r.status_code == 400 and r.json()["detail"]["key"] == "err.unknown_memory"
    assert client.post("/api/settings", json={"memory": "fast"}, headers=H).status_code == 200
    assert store._keep_vectors
    assert "app_mb" in client.get("/api/status").json()["memory"]


def test_an_unchanged_map_is_not_sent_again(client):
    import time

    for _ in range(100):  # an empty index: laid out at once
        r = client.get("/api/map")
        if r.json()["status"] == "ready":
            break
        time.sleep(0.05)
    tag = r.headers["etag"]
    again = client.get("/api/map", headers={"If-None-Match": tag})
    assert again.status_code == 304 and not again.content


def test_similar_files_work_while_the_model_sleeps(client):
    import numpy as np

    store = client.backend.store
    folder = store.add_folder(r"C:\data")
    for i in range(3):
        v = np.random.default_rng(i).standard_normal(256).astype(np.float32)
        path = rf"C:\data\f{i}.txt"
        store.save_files([{"folder_id": folder, "path": path, "size": 1, "mtime": 1.0, "kind": "text",
                           "status": "indexed", "chunks": 1, "error": None}],
                         [{"path": path, "folder_id": folder, "chunk": 0, "kind": "text", "text": "t",
                           "vector": (v / np.linalg.norm(v)).tolist()}])
    assert not client.backend.embedder.ready
    r = client.get(f"/api/similar/{store.signature(r'C:\data\f0.txt')[0]}")
    assert r.status_code == 200 and len(r.json()["results"]) == 2


def test_unreadable_files_warn_once_and_stay_listed(client):
    store = client.backend.store
    folder = store.add_folder(r"C:\pics")
    store.save_files([{"folder_id": folder, "path": r"C:\pics\a.jpg", "size": 1, "mtime": 1.0, "kind": "image",
                       "status": "error", "chunks": 0, "error": "!damaged_image"}], [])
    folders = lambda: client.get("/api/status").json()["folders"][0]  # noqa: E731
    assert (folders()["errors"], folders()["unreadable"]) == (1, 1)
    assert client.post(f"/api/folders/{folder}/errors/seen", headers=H).status_code == 200
    assert (folders()["errors"], folders()["unreadable"]) == (0, 1)
    rows = client.get(f"/api/folders/{folder}/errors").json()
    assert [(r["path"], r["error"], r["seen"]) for r in rows] == [(r"C:\pics\a.jpg", "!damaged_image", True)]
    assert client.post(f"/api/folders/{folder}/rescan", headers=H).status_code == 200
    assert folders()["errors"] == 0  # trying again doesn't bring the warning back
    assert client.post("/api/folders/999/errors/seen", headers=H).status_code == 404
