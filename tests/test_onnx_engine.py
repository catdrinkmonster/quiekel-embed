"""The ONNX engine: pictures prepared like the model's own processor, the model fed right."""

import numpy as np
import pytest
from PIL import Image

from quiekel_embed import config, embedder, onnx_engine


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


def test_the_engine_follows_whats_installed(monkeypatch):
    monkeypatch.setenv("QUIEKEL_EMBED_ENGINE", "onnx")
    assert embedder._wanted_engine() == "onnx"
    monkeypatch.setenv("QUIEKEL_EMBED_ENGINE", "torch")
    assert embedder._wanted_engine() == "torch"


def test_directml_running_out_of_video_memory_counts_as_such():
    assert embedder._is_oom(RuntimeError("Non-zero status code returned: E_OUTOFMEMORY (0x8007000E)"))
    assert not embedder._is_oom(RuntimeError("some other failure"))


@pytest.mark.skipif(not (onnx_engine.available() and onnx_engine.cached(False)), reason="needs the ONNX model files")
def test_the_real_model_on_the_processor(monkeypatch):
    monkeypatch.setenv("QUIEKEL_EMBED_ENGINE", "onnx")
    e = embedder.Embedder()
    e.load(prefer_gpu=False)
    assert (e.engine, e.on_gpu, e.cuda) == ("cpu", False, False)
    docs = e.documents(["title: Mietvertrag | text: Die Miete beträgt 800 Euro im Monat.",
                        "title: main.py | text: def add(a, b): return a + b"])
    q = e.query("letter from the landlord about the rent")
    assert q @ docs[0] > q @ docs[1]
