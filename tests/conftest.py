import threading

import pytest
from fastapi.testclient import TestClient

from quiekel_embed import config


@pytest.fixture
def client(tmp_path, monkeypatch):
    """The web app on a fresh, empty index. The model is never loaded."""
    for name, value in (("DATA_DIR", tmp_path), ("DB_PATH", tmp_path / "state.db"),
                        ("VECTORS_DIR", tmp_path / "vectors"), ("THUMBS_DIR", tmp_path / "thumbs")):
        monkeypatch.setattr(config, name, value)
    from quiekel_embed.app import Backend, Hooks, create_app
    from quiekel_embed.embedder import Embedder
    from quiekel_embed.governor import Decision, Governor, Metrics
    from quiekel_embed.indexer import Indexer
    from quiekel_embed.store import Settings, Store
    from quiekel_embed.updater import Updater

    store = Store()
    settings = Settings(store)
    governor = Governor.__new__(Governor)  # no sampling thread
    governor.metrics, governor.decision = Metrics(), Decision(1.0, "full", "full", {})
    governor._lock, governor._get_mode = threading.Lock(), lambda: settings.get("perf_mode")
    embedder = Embedder()  # never loaded: these tests don't need the model
    backend = Backend(store, settings, governor, embedder, Indexer(store, embedder, governor, settings),
                      Updater(lambda: False))
    client = TestClient(create_app(backend, Hooks()), base_url=f"http://127.0.0.1:{config.PORT}")
    client.backend = backend  # for tests that put files in the index
    return client
