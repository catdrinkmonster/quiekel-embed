def test_pages_get_strict_security_headers(client):
    r = client.get("/")
    assert r.status_code == 200
    assert "script-src 'self'" in r.headers["content-security-policy"]
    assert r.headers["x-frame-options"] == "DENY"
    assert r.headers["cross-origin-resource-policy"] == "same-origin"


def test_other_hosts_and_sites_are_refused(client):
    assert client.get("/api/status", headers={"host": "evil.example"}).status_code == 403
    # Another web page (even another localhost port) trying to read or embed our data:
    assert client.get("/api/status", headers={"sec-fetch-site": "cross-site"}).status_code == 403
    assert client.get("/api/thumb/1", headers={"sec-fetch-site": "same-site"}).status_code == 403
    assert client.get("/api/status", headers={"sec-fetch-site": "same-origin"}).status_code == 200


def test_changes_need_the_app_header(client):
    assert client.post("/api/pause").status_code == 403  # e.g. a form post from a web page
    assert client.post("/api/pause", headers={"x-quiekel-embed": "1"}).status_code == 200


def test_updates_only_install_from_the_desktop_app(client):
    r = client.post("/api/update/apply", headers={"x-quiekel-embed": "1"})
    assert r.status_code == 400


def test_errors_come_as_translation_keys(client):
    r = client.post("/api/folders", json={"path": "  "}, headers={"x-quiekel-embed": "1"})
    assert r.status_code == 400
    detail = r.json()["detail"]
    assert detail["key"] == "err.empty_path" and detail["message"] == "Enter a folder path."


def test_language_setting(client):
    h = {"x-quiekel-embed": "1"}
    r = client.post("/api/settings", json={"language": "de"}, headers=h)
    assert r.status_code == 200 and r.json()["language_resolved"] == "de"
    assert client.get("/api/status").json()["settings"]["language"] == "de"
    r = client.post("/api/settings", json={"language": "klingon"}, headers=h)
    assert r.status_code == 400 and r.json()["detail"]["key"] == "err.unknown_language"
