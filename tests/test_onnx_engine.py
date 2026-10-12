"""The ONNX engine: pictures prepared like the model's own processor, the model fed right."""

import os

import numpy as np
import pytest
from PIL import Image

from quiekel_embed import config, embedder, machine, model_process, onnx_engine


@pytest.mark.parametrize("size, expected", [
    ((427, 640), (624, 960)), ((150, 200), (672, 912)), ((1200, 300), (1584, 384)), ((100, 4000), (96, 5040)),
    ((4000, 100), (5040, 96)), ((20, 20), (768, 768)), ((3000, 4000), (672, 912)), ((1080, 1920), (576, 1056)),
])
def test_pictures_are_sized_like_the_models_own_processor(size, expected):
    assert onnx_engine.target_size(*size) == expected  # (values from the original's function)


def test_pictures_become_patches_with_their_places():
    image = Image.new("RGB", (200, 150), (0, 0, 0))
    image.putpixel((0, 0), (255, 0, 0))
    pixels, places, soft = onnx_engine.patches(image)  # 200 x 150 -> 912 x 672: 57 x 42 patches
    found = 57 * 42
    assert pixels.shape == (onnx_engine.MAX_PATCHES, 768) and places.shape == (onnx_engine.MAX_PATCHES, 2)
    assert soft == found // 9
    assert places[:2].tolist() == [[0, 0], [1, 0]] and places[57].tolist() == [0, 1]  # (column, row)
    assert (places[found:] == -1).all() and not pixels[found:].any()  # padding
    assert pixels[0, 0] > 0.5 and pixels[0, 1] < 0.5  # first patch, first pixel: red, then green
    assert onnx_engine.picture_ids(2) == [2, 255999, 258880, 258880, 258882, 1]


class FakeSession:
    def __init__(self, outputs):
        self.outputs, self.feeds = outputs, []

    def run(self, names, feeds):
        self.feeds.append(feeds)
        return [self.outputs(feeds)]


class FakeTokenizer:
    def encode_batch(self, texts):
        return [type("Encoding", (), {"ids": [2] + [ord(c) for c in t] + [1]})() for t in texts]


@pytest.fixture
def model():
    m = object.__new__(onnx_engine.OnnxModel)  # no model files needed
    m.gpu = False
    m.tokenizer = FakeTokenizer()
    m.text = FakeSession(lambda feeds: np.tile(np.arange(1, 769, dtype=np.float32), (len(feeds["input_ids"]), 1)))
    m._vision = FakeSession(lambda feeds: np.ones((int((feeds["pixel_position_ids"][..., 0] >= 0).sum()) // 9, 512),
                                                  np.float32))
    return m


def test_texts_are_padded_and_the_output_cut_and_normalized(model):
    features = model.preprocess(["ab", "abcd"], prompt="q: ")
    out = model.embed(features)
    feeds = model.text.feeds[0]
    assert feeds["input_ids"].tolist() == [[2, 113, 58, 32, 97, 98, 1, 0, 0], [2, 113, 58, 32, 97, 98, 99, 100, 1]]
    assert feeds["attention_mask"].tolist() == [[1] * 7 + [0, 0], [1] * 9]
    assert feeds["image_features"].shape == (0, 512)
    assert out.shape == (2, config.EMBED_DIM) and np.allclose(np.linalg.norm(out, axis=1), 1.0)


def test_on_directml_texts_come_in_a_few_lengths_only(model):
    model.gpu = True  # (it prepares its work anew for every new length)
    out = model.embed(model.preprocess(["ab", "abcd"], prompt="q: "))
    feeds = model.text.feeds[0]
    assert feeds["input_ids"].shape == (2, model.STEP)
    assert feeds["attention_mask"].sum(axis=1).tolist() == [7, 9]
    assert np.allclose(np.linalg.norm(out, axis=1), 1.0)


def test_directml_gets_a_copy_of_the_graph_it_can_run(tmp_path):
    zero = onnx_engine._ALLOWZERO[:-1] + bytes(1)
    graph = tmp_path / "model_fp16.onnx"
    graph.write_bytes(b"head" + onnx_engine._ALLOWZERO + b"middle" + onnx_engine._ALLOWZERO + b"tail")
    fixed = onnx_engine.for_directml(graph)
    assert fixed == tmp_path / "model_fp16_directml.onnx"  # next to the weights it shares
    assert fixed.read_bytes() == b"head" + zero + b"middle" + zero + b"tail"
    assert onnx_engine.for_directml(fixed) == fixed  # nothing left to change
    plain = tmp_path / "vision_encoder_fp16.onnx"
    plain.write_bytes(b"no reshape with allowzero")
    assert onnx_engine.for_directml(plain) == plain


def test_pictures_go_through_the_vision_encoder_first(model):
    features = model.preprocess([{"image": Image.new("RGB", (200, 150))}, {"image": Image.new("RGB", (64, 64))}])
    model.embed(features)
    feeds = model.text.feeds[0]
    soft = [ids.count(onnx_engine.IMAGE) for ids in features["ids"]]
    assert feeds["image_features"].shape == (sum(soft), 512)  # one row per picture slot, in order
    assert feeds["input_ids"].shape == (2, max(soft) + 4)


def test_linked_model_files_become_files_in_the_models_folder(tmp_path):
    blob = tmp_path / "blobs" / "cbc6396c"  # (the cache's one copy)
    blob.parent.mkdir()
    blob.write_bytes(b"weights")
    folder = tmp_path / "snapshot"
    (folder / "onnx").mkdir(parents=True)
    link = folder / "onnx" / "model_quantized.onnx_data"
    try:
        link.symlink_to(blob)
    except OSError:
        pytest.skip("Windows allows no links here (no Developer Mode, not an administrator)")
    onnx_engine.as_real_files(folder, ["onnx/model_quantized.onnx_data", "onnx/not_downloaded.onnx"])
    assert not link.is_symlink() and link.read_bytes() == b"weights"  # ONNX Runtime takes it now
    assert blob.read_bytes() == b"weights" and list((folder / "onnx").iterdir()) == [link]


def test_directml_running_out_of_video_memory_counts_as_such():
    assert model_process.is_oom(RuntimeError("Non-zero status code returned: E_OUTOFMEMORY (0x8007000E)"))
    assert model_process.is_oom(RuntimeError("bad allocation")) and model_process.is_oom(MemoryError())
    assert not model_process.is_oom(RuntimeError("some other failure"))


# ---- in the model process: whose fault a failure is ----------------------------------------


class FakeOnnx:
    """Stands in for OnnxModel: fails the given ways first, then works."""

    prompts = {"SearchQuery": "q: "}

    def __init__(self, errors=(), prep_error=None):
        self.errors, self.prep_error = list(errors), prep_error

    def preprocess(self, items, prompt=None):
        if self.prep_error:
            raise self.prep_error
        return {"items": list(items)}

    def embed(self, features):
        if self.errors:
            raise self.errors.pop(0)
        out = np.zeros((len(features["items"]), config.EMBED_DIM), np.float32)
        out[:, 0] = 1
        return out


def test_any_directml_failure_counts_as_running_out_of_memory():
    # On German Windows onnxruntime once couldn't even decode DirectML's error message.
    unreadable = UnicodeDecodeError("utf-8", bytes([0xFC]), 0, 1, "invalid start byte")
    for error in (unreadable, RuntimeError("80004005 Unbekannter Fehler")):
        reply = model_process._answer(FakeOnnx([error]), ("embed", "text", ["a"], ""), gpu=True)
        assert reply[:2] == ("error", "oom")


def test_on_the_processor_an_engine_failure_is_the_engines():
    reply = model_process._answer(FakeOnnx([RuntimeError("something inside broke")]), ("embed", "text", ["a"], ""), gpu=False)
    assert reply[:2] == ("error", "engine")


def test_an_item_that_cant_be_prepared_is_the_files_fault():
    reply = model_process._answer(FakeOnnx(prep_error=ValueError("not a picture")), ("embed", "text", ["a"], ""), gpu=False)
    assert reply[:2] == ("error", "input")
    reply = model_process._answer(FakeOnnx(prep_error=MemoryError()), ("embed", "text", ["a"], ""), gpu=False)
    assert reply[:2] == ("error", "oom")  # (too big to prepare now: not the file's fault)


def test_pictures_cross_over_as_plain_pixels():
    reply = model_process._answer(FakeOnnx(), ("embed", "image", [np.zeros((40, 30, 3), np.uint8)], None), gpu=False)
    assert reply[0] == "ok" and reply[1].shape == (1, config.EMBED_DIM)


# ---- the real model, in its own process ----------------------------------------------------

real_model = pytest.mark.skipif(not onnx_engine.cached(False), reason="needs the model files (8-bit)")
# The graphics card only when asked: it may be busy with something else (QUIEKEL_TEST_GPU=1).
real_gpu = pytest.mark.skipif(os.environ.get("QUIEKEL_TEST_GPU") != "1" or not onnx_engine.cached(True)
                              or not machine.graphics_cards(), reason="QUIEKEL_TEST_GPU=1, a graphics card, its model files")

DOCS = ["title: Mietvertrag | text: Die Miete beträgt 800 Euro im Monat und ist am Ersten fällig.",
        "title: main.py | text: def add(a, b): return a + b",
        "title: Linsensuppe | text: Linsen, Karotten und Zwiebeln eine halbe Stunde kochen lassen."]


def photo() -> Image.Image:
    image = Image.new("RGB", (1600, 1200), (70, 140, 210))  # sky, sea and sand, photo-sized
    image.paste((20, 80, 160), (0, 600, 1600, 900))
    image.paste((230, 200, 140), (0, 900, 1600, 1200))
    return image


def started(profile: machine.Profile) -> embedder.Embedder:
    e = embedder.Embedder(profile=profile)
    e.load()
    return e


@real_model
@pytest.mark.parametrize("what, profile, setup, threads", [
    ("desktop without a graphics card", machine.Profile(16, 8, ()), "processor", 4),
    ("built-in graphics, 8 GB", machine.Profile(8, 4, (machine.Card(0, "Built-in", 128 * 10**6),), threads_cap=4),
     "light", 2),
    ("old dual core, 4 GB", machine.Profile(4, 2, (), threads_cap=2), "light", 1),
], ids=["desktop", "built-in-8gb", "dual-core-4gb"])
def test_the_real_model_on_pcs_without_a_graphics_card_to_use(what, profile, setup, threads):
    e = started(profile)
    try:
        assert (e.setup.name, e.setup.threads, e.on_gpu) == (setup, threads, False), what
        docs = e.documents(DOCS)
        assert np.argmax(docs @ e.query("when is the rent due")) == 0
        assert np.argmax(docs @ e.query("soup recipe")) == 2
        assert e.images([photo()]).shape == (1, config.EMBED_DIM)
        assert e.memory_mb() < e.setup.memory_cap_gb * 1000  # (Windows holds it to that)
    finally:
        e.unload("test over")


@real_model
def test_every_setup_on_the_processor_gives_the_same_vectors():
    """One index for all: made with one setup, searched with another (a new PC, fewer threads)."""
    out = []
    for profile in (machine.Profile(16, 8, ()), machine.Profile(4, 2, (), threads_cap=2)):
        e = started(profile)
        try:
            out.append(e.documents(DOCS))
        finally:
            e.unload("test over")
    assert np.allclose(out[0], out[1], atol=1e-3)


@real_model
@real_gpu
def test_the_real_model_on_the_graphics_card():
    e = started(machine.Profile(32, 8, tuple(machine.graphics_cards())))
    try:
        assert (e.setup.name, e.on_gpu) == ("graphics", True)
        docs = e.documents(DOCS)
        assert np.argmax(docs @ e.query("when is the rent due")) == 0
        assert e.images([photo()]).shape == (1, config.EMBED_DIM)
    finally:
        e.unload("test over")
    cpu = started(machine.Profile(16, 8, ()))
    try:
        # Half precision on the card, 8-bit on the processor: close enough to search each other's index.
        assert (np.sum(docs * cpu.documents(DOCS), axis=1) > 0.97).all()
    finally:
        cpu.unload("test over")
