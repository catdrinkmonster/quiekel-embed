import numpy as np
import pytest

from quiekel_embed.filemap import TSNE_MIN, per_file, to_3d


def test_one_unit_vector_per_file():
    ids = np.array([3, 1, 3, 2])
    vecs = np.array([[1, 0], [0, 2], [0, 1], [3, 3]], dtype=np.float32)
    files, means = per_file(ids, vecs)
    assert files.tolist() == [1, 2, 3]
    assert np.allclose(np.linalg.norm(means, axis=1), 1)
    assert np.allclose(means[2], [2 ** -0.5, 2 ** -0.5])  # mean of (1,0) and (0,1)


@pytest.mark.parametrize("per_group", [20, 60])  # PCA alone, and PCA + t-SNE
def test_similar_vectors_land_close_together(per_group):
    rng = np.random.default_rng(1)
    centers = rng.normal(size=(3, 256))
    vecs = np.concatenate([c + 0.05 * rng.normal(size=(per_group, 256)) for c in centers])
    vecs /= np.linalg.norm(vecs, axis=1, keepdims=True)
    coords = to_3d(vecs)
    assert coords.shape == (3 * per_group, 3) and np.isfinite(coords).all()
    assert np.abs(coords).max() <= 1.15
    # Each point's nearest neighbours on the map belong to its own group.
    group = np.repeat(np.arange(3), per_group)
    dist = np.linalg.norm(coords[:, None] - coords[None], axis=2)
    np.fill_diagonal(dist, np.inf)
    nearest = np.argsort(dist, axis=1)[:, :3]
    assert (group[nearest] == group[:, None]).mean() > 0.95
    assert (3 * per_group >= TSNE_MIN) == (per_group == 60)  # both paths are covered


def test_tiny_indexes():
    assert to_3d(np.zeros((0, 256), np.float32)).shape == (0, 3)
    assert to_3d(np.ones((1, 256), np.float32)).tolist() == [[0.0, 0.0, 0.0]]
    assert to_3d(np.random.default_rng(0).normal(size=(5, 256))).shape == (5, 3)
