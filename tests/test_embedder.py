"""The app's side of the model: batches, memory, and what happens when the model process fails.
These start a real model process with the stand-in model (model_process.StandIn)."""

import numpy as np
import psutil
import pytest

from quiekel_embed import config, embedder, machine

PC = machine.Profile(ram_gb=16, cores=8, cards=())  # the processor setup: batches of 8
SMALL = machine.Profile(ram_gb=8, cores=4, cards=(), threads_cap=2)  # the light setup: batches of 4


def started(profile=PC) -> embedder.Embedder:
    e = embedder.Embedder(profile=profile, model="stand-in")
    e.load()
    return e


@pytest.fixture
def e():
    model = started()
    yield model
    model.unload("test over")


@pytest.fixture
def small():
    model = started(SMALL)
    yield model
    model.unload("test over")


def each(e, texts) -> np.ndarray:
    pace, e.pace = e.pace, None
    try:
        return np.stack([e.documents([t])[0] for t in texts])
    finally:
        e.pace = pace


def test_results_come_back_in_input_order_though_batches_are_sorted(small):
    texts = ["a" * n for n in (5, 1, 9, 3, 7, 2, 8)]
    paced = []
    small.pace = paced.append
    out = small.documents(texts)
    assert out.shape == (7, config.EMBED_DIM) and np.allclose(np.linalg.norm(out, axis=1), 1.0)
    assert np.allclose(out, each(small, texts))
    assert len(paced) == 2  # two batches of up to 4: the governor gets a say after each


def test_batches_shrink_when_memory_runs_out(e):
    texts = ["!oom-over:2"] + [f"note {i}" for i in range(7)]  # fails in a batch of more than 2
    assert np.allclose(e.documents(texts), each(e, texts))


def test_a_single_item_that_does_not_fit_gives_up(e):
    with pytest.raises(embedder.GpuBusy):
        e.documents(["!oom"])


def test_windows_holds_the_model_to_its_memory_cap(small):
    with pytest.raises(embedder.GpuBusy):
        small.documents(["!alloc:2000"])  # 2 GB in a process capped at 1 GB
    assert small.ready and small.documents(["!alloc:200"]).shape == (1, config.EMBED_DIM)


def test_an_item_that_cant_be_prepared_is_that_files_fault(e):
    with pytest.raises(embedder.BadInput):
        e.documents(["!bad one"])
    assert e.ready


def test_a_crash_is_the_engines_fault_and_it_starts_again(e):
    with pytest.raises(embedder.EngineError):
        e.documents(["!crash"])
    assert not e.ready and e.status == "asleep"
    e.load()
    assert e.documents(["back"]).shape == (1, config.EMBED_DIM)


def test_letting_the_model_go_between_batches_is_no_error(small):
    small.pace = lambda seconds: small.unload("a game started")
    with pytest.raises(embedder.ModelAsleep):
        small.documents([f"note {i}" for i in range(8)])
    assert small.status == "asleep"


def test_stopping_ends_the_model_process(e):
    process = e._proc
    e.unload("not used")
    assert e.pid is None and process._popen.poll() is not None  # (ended: all its memory is back)


def test_the_model_only_starts_with_memory_to_spare(monkeypatch):
    monkeypatch.setattr(machine, "free_ram_gb", lambda: 0.8)
    e = embedder.Embedder(profile=PC, model="stand-in")
    with pytest.raises(embedder.NotEnoughMemory):
        e.load()
    assert e.status == "asleep" and not e.ready


def test_a_graphics_card_that_wont_start_hands_over_to_the_processor():
    card = machine.Profile(ram_gb=16, cores=8, cards=(machine.Card(0, "!broken card", 8 * 10**9),))
    e = started(card)
    try:
        assert (e.setup.name, e.engine, e.gpu_trouble) == ("processor", "cpu", True)
    finally:
        e.unload("test over")


def test_a_crash_on_the_graphics_card_hands_over_to_the_processor():
    card = machine.Profile(ram_gb=16, cores=8, cards=(machine.Card(0, "Card", 8 * 10**9),))
    e = started(card)
    try:
        assert e.setup.name == "graphics" and e.on_gpu
        with pytest.raises(embedder.EngineError):
            e.documents(["!crash"])
        e.load()
        assert (e.setup.name, e.on_gpu) == ("processor", False)
    finally:
        e.unload("test over")


def test_search_queries_are_answered(e):
    v = e.query("rent contract")
    assert v.shape == (config.EMBED_DIM,) and abs(np.linalg.norm(v) - 1) < 1e-5


def test_the_model_runs_below_normal_priority_with_its_setups_threads(small):
    proc = psutil.Process(small.pid)
    assert proc.nice() == psutil.BELOW_NORMAL_PRIORITY_CLASS
    assert proc.cpu_affinity() == [0, 1]  # (a pretended PC with two threads for the model)
    assert small.memory_mb() > 0


def test_nothing_to_embed(e):
    assert e.documents([]).shape == (0, config.EMBED_DIM)


@pytest.mark.parametrize("profile, prefer_gpu, why", [
    (machine.Profile(ram_gb=16, cores=8, cards=()), True, "none"),
    (machine.Profile(ram_gb=8, cores=4, cards=(machine.Card(0, "Built-in", 128 * 10**6),)), True, "shared"),
    (machine.Profile(ram_gb=16, cores=8, cards=(machine.Card(0, "Card", 8 * 10**9),)), False, "busy"),
    (machine.Profile(ram_gb=16, cores=8, cards=(machine.Card(0, "!broken card", 8 * 10**9),)), True, "failed"),
])
def test_the_page_learns_why_the_model_is_on_the_processor(profile, prefer_gpu, why):
    e = embedder.Embedder(profile=profile, model="stand-in")
    e.load(prefer_gpu=prefer_gpu)
    try:
        assert (e.on_gpu, e.cpu_because) == (False, why)
        assert e.could_use_gpu == (why == "busy")  # (only a busy card is worth going back to)
    finally:
        e.unload("test over")


def test_a_model_process_that_hangs_is_ended(e, monkeypatch):
    from quiekel_embed import model_process

    monkeypatch.setattr(model_process, "REQUEST_TIMEOUT_S", 1.0)
    with pytest.raises(embedder.EngineError):
        e.documents(["!alloc:1", "!hang"])
    assert not e.ready

