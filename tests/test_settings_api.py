H = {"x-quiekel-embed": "1"}


def settings(client):
    return client.get("/api/status").json()["settings"]


def test_defaults_ask_before_going_online(client):
    s = settings(client)
    assert s["check_updates"] is False  # update checks are opt-in
    assert (s["types_off"], s["types_added"], s["attachments"], s["big_archives"]) == ([], [], True, False)
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


def test_light_performance_keeps_search_data_on_disk(client):
    store = client.backend.store
    assert settings(client)["perf_mode"] == "balanced" and store._keep_vectors
    assert client.post("/api/settings", json={"perf_mode": "gentle"}, headers=H).status_code == 200
    assert not store.vectors.ready and not store._keep_vectors
    r = client.post("/api/settings", json={"perf_mode": "turbo"}, headers=H)
    assert r.status_code == 400 and r.json()["detail"]["key"] == "err.unknown_mode"
    assert client.post("/api/settings", json={"perf_mode": "full"}, headers=H).status_code == 200
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


def test_file_types_menu(client):
    menu = client.get("/api/filetypes").json()
    groups = {g["key"]: g["exts"] for g in menu["groups"]}
    assert ".odt" in groups["doc"] and ".msg" in groups["mail"] and ".7z" in groups["archive"]
    assert menu["rules"] == menu["defaults"] and menu["off"] == menu["added"] == []

    r = client.post("/api/settings", json={"types_off": [".JSON", "txt"], "types_added": ["XYZ"],
                                           "big_archives": True}, headers=H)
    assert r.status_code == 200
    s = r.json()
    assert (s["types_off"], s["types_added"], s["big_archives"]) == ([".json", ".txt"], [".xyz"], True)
    bad = client.post("/api/settings", json={"types_added": ["../evil"]}, headers=H)
    assert bad.status_code == 400 and bad.json()["detail"]["key"] == "err.unknown_type"


def test_folder_details_say_why_files_are_not_searched(client):
    store = client.backend.store
    folder = store.add_folder(r"C:\docs")
    store.save_files([{"folder_id": folder, "path": r"C:\docs\leer.txt", "size": 0, "mtime": 1.0, "kind": "text",
                       "status": "skipped", "chunks": 0, "error": "!empty"}], [])
    store.mark_scanned(folder, {"media": {"n": 2, "examples": [r"C:\docs\a.mp4"], "exts": {".mp4": 2}}})
    d = client.get(f"/api/folders/{folder}/details").json()
    assert d["report"]["media"]["n"] == 2
    assert [(s["reason"], s["n"], s["examples"][0]["path"]) for s in d["skipped"]] == [("!empty", 1, r"C:\docs\leer.txt")]
    assert d["errors"] == []
    assert client.get("/api/status").json()["folders"][0]["unsearched"] == 3
    assert client.get("/api/folders/999/details").status_code == 404


def test_files_left_out_are_shown_by_where_the_details_name_them(client, tmp_path, monkeypatch):
    import subprocess

    store = client.backend.store
    folder = store.add_folder(str(tmp_path))
    video = tmp_path / "urlaub.mp4"
    video.write_bytes(b"x")
    store.mark_scanned(folder, {"media": {"n": 1, "examples": [str(video)], "exts": {".mp4": 1}}})
    opened = []
    monkeypatch.setattr(subprocess, "Popen", lambda cmd, *a, **k: opened.append(cmd))
    reveal = lambda reason, index: client.post(f"/api/folders/{folder}/reveal",  # noqa: E731
                                               json={"reason": reason, "index": index}, headers=H).status_code
    assert reveal("media", 0) == 200 and opened == [f'explorer /select,"{video}"']
    assert reveal("media", 1) == reveal("type", 0) == reveal("media", -1) == 404  # only what the details name
