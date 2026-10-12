"""Starting the app: one copy at a time, and its priority."""

import os

import psutil
import pytest

windows = pytest.mark.skipif(os.name != "nt", reason="Windows only")


@windows
def test_a_second_copy_notices_the_first_right_away(monkeypatch):
    from quiekel_embed import desktop

    monkeypatch.setattr(desktop.config, "APP_ID", f"QuiekelEmbed.Test.{os.getpid()}")
    monkeypatch.setattr(desktop, "_INSTANCE", [])
    assert desktop._first_copy() is True
    assert desktop._first_copy() is False  # (while the server's port isn't even open yet)


@windows
def test_a_copy_with_its_own_data_folder_runs_next_to_the_app(monkeypatch, tmp_path):
    from quiekel_embed import desktop

    monkeypatch.setattr(desktop.config, "APP_ID", f"QuiekelEmbed.Test.{os.getpid()}.copies")
    monkeypatch.setattr(desktop, "_INSTANCE", [])
    assert desktop._first_copy() is True  # the app
    monkeypatch.setattr(desktop.config, "COPY_NAME", "Old dual core, 4 GB")
    monkeypatch.setattr(desktop.config, "DATA_DIR", tmp_path / "Old dual core, 4 GB")
    assert desktop._first_copy() is True  # a test copy, next to it
    assert desktop._first_copy() is False  # (but only one of those)


@windows
def test_programs_started_from_the_app_get_normal_priority():
    from quiekel_embed import app

    me = psutil.Process()
    before = me.nice()
    try:
        app.lower_priority()
        assert me.nice() == psutil.BELOW_NORMAL_PRIORITY_CLASS
        with app.normal_priority():
            assert me.nice() == psutil.NORMAL_PRIORITY_CLASS
        assert me.nice() == psutil.BELOW_NORMAL_PRIORITY_CLASS
    finally:
        me.nice(before)
