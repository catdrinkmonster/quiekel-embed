import numpy as np
import pytest

from quiekel_embed import config
from quiekel_embed.embedder import Embedder, GpuBusy

torch = pytest.importorskip("torch")  # CI installs everything but PyTorch (its CUDA build is huge)


class FakeModel:
    """Stands in for the SentenceTransformer: the text "aaa" becomes a unit vector along axis 3."""

    truncate_dim = config.EMBED_DIM
    device = torch.device("cpu")
    prompts = {"SearchQuery": "task: search result | query: "}

    def __init__(self, max_batch=None):
        self.max_batch = max_batch
        self.batches = []
        self.prompts_seen = set()

    def _input_length(self, x):
        return len(x)

    def preprocess(self, items, prompt=None, **kwargs):
        self.prompts_seen.add(prompt)
        return {"lengths": torch.tensor([len(x) for x in items])}

    def __call__(self, features):
        lengths = features["lengths"]
        if self.max_batch is not None and len(lengths) > self.max_batch:
            raise RuntimeError("CUDA out of memory. Tried to allocate 2.00 GiB")
        self.batches.append(lengths.tolist())
        out = torch.zeros(len(lengths), config.EMBED_DIM + 8)
        out[torch.arange(len(lengths)), lengths] = 1.0
        return {"sentence_embedding": out}


@pytest.fixture
def embedder():
    e = Embedder()
    e._model = FakeModel()
    return e


def test_results_come_back_in_input_order_though_batches_are_sorted(embedder):
    texts = ["a" * n for n in (5, 1, 9, 3, 7, 2, 8)]
    paced = []
    embedder.pace = paced.append
    out = embedder._embed(texts, base=3, prompt="")
    assert [int(v.argmax()) for v in out] == [5, 1, 9, 3, 7, 2, 8]
    assert np.allclose(np.linalg.norm(out, axis=1), 1.0)
    assert out.shape == (7, config.EMBED_DIM)
    assert embedder._model.batches == [[9, 8, 7], [5, 3, 2], [1]]  # longest first: little padding
    assert len(paced) == 3  # the governor gets a say after every batch


def test_batches_shrink_when_video_memory_runs_out(embedder):
    embedder._model.max_batch = 2
    texts = ["a" * n for n in range(1, 8)]
    out = embedder._embed(texts, base=4, prompt="")
    assert [int(v.argmax()) for v in out] == list(range(1, 8))
    assert all(len(b) <= 2 for b in embedder._model.batches)


def test_a_single_item_that_does_not_fit_gives_up(embedder):
    embedder._model.max_batch = 0
    with pytest.raises(GpuBusy):
        embedder._embed(["aaa"], base=4, prompt="")


def test_search_queries_use_the_query_prompt(embedder):
    v = embedder.query("aaaa")
    assert int(v.argmax()) == 4
    assert embedder._model.prompts_seen == {"task: search result | query: "}


def test_nothing_to_embed(embedder):
    assert embedder._embed([], base=4).shape == (0, config.EMBED_DIM)
