"""EmbeddingGemma 2 through ONNX Runtime, for PCs without an NVIDIA card: on any graphics card
Windows can drive (DirectML: AMD, Intel, also NVIDIA), or on the processor. It needs no PyTorch,
2.5 GB less to download.

The model is onnx-community's conversion of the same EmbeddingGemma 2, pinned to one revision.
It matches the original closely (cosine similarity 0.9999 for text, 0.999 for pictures), so an
index made with one engine works with the other. Pictures are prepared here as the original
processor does: resized to fit 2,520 patches of 16 x 16 pixels, cut into those patches, and each
patch's place noted.
"""

import math
import os
from pathlib import Path

import numpy as np
from PIL import Image

from . import config

REPO = "onnx-community/embeddinggemma-2-ONNX"
REVISION = "daa72c51243991dfcaf9f9137d2c573d8f7790c0"  # these exact files
BOS, EOS, PAD = 2, 1, 0  # the tokenizer's (it adds BOS and EOS itself)
BOI, IMAGE, EOI = 255999, 258880, 258882  # a picture: BOI, one IMAGE per soft token, EOI
PATCH, POOL, SOFT_TOKENS = 16, 3, 280
MAX_PATCHES = SOFT_TOKENS * POOL * POOL
MAX_TOKENS = 2048  # passages are far shorter; this only stops a runaway one
QUERY_PROMPT = "task: search result | query: "
_NO_FEATURES = np.zeros((0, 512), np.float32)


def files(gpu: bool) -> list[str]:
    """What a graphics card (half precision) or the processor (8-bit) needs."""
    flavour = "fp16" if gpu else "quantized"
    return ["config.json", "tokenizer.json"] + [
        f"onnx/{part}_{flavour}.onnx{ext}" for part in ("model", "vision_encoder") for ext in ("", "_data")]


def snapshot() -> Path:
    home = Path(os.environ.get("HF_HOME", Path.home() / ".cache" / "huggingface"))
    hub = Path(os.environ.get("HF_HUB_CACHE", home / "hub"))
    return hub / f"models--{REPO.replace('/', '--')}" / "snapshots" / REVISION


def cached(gpu: bool) -> bool:
    return all((snapshot() / f).is_file() for f in files(gpu))


def download(gpu: bool) -> Path:
    if cached(gpu):
        return snapshot()
    from huggingface_hub import snapshot_download

    return Path(snapshot_download(REPO, revision=REVISION, allow_patterns=files(gpu)))


def available() -> bool:
    import importlib.util

    return importlib.util.find_spec("onnxruntime") is not None


def directml() -> bool:
    import onnxruntime as ort

    return "DmlExecutionProvider" in ort.get_available_providers()


# ---- pictures, as the model's own processor prepares them ---------------------------------


def target_size(height: int, width: int) -> tuple[int, int]:
    """The largest size with the same shape that fits the patch budget, in steps of 48 pixels."""
    factor = math.sqrt(MAX_PATCHES * PATCH * PATCH / (height * width))
    side = POOL * PATCH
    h = int(math.floor(factor * height / side)) * side
    w = int(math.floor(factor * width / side)) * side
    longest = (MAX_PATCHES // POOL ** 2) * side
    if h == 0:
        h, w = side, min(int(math.floor(width / height)) * side, longest)
    elif w == 0:
        w, h = side, min(int(math.floor(height / width)) * side, longest)
    return h, w


def patches(image: Image.Image) -> tuple[np.ndarray, np.ndarray, int]:
    """(pixels of each patch, padded to MAX_PATCHES; each patch's (column, row), -1 for padding;
    the number of soft tokens the picture becomes)."""
    image = image.convert("RGB")
    h, w = target_size(image.height, image.width)
    if (h, w) != (image.height, image.width):
        image = image.resize((w, h), Image.BICUBIC)
    a = np.asarray(image, np.float32).transpose(2, 0, 1) / 255.0  # (3, h, w)
    rows, cols = h // PATCH, w // PATCH
    found = a.reshape(3, rows, PATCH, cols, PATCH).transpose(1, 3, 2, 4, 0).reshape(rows * cols, -1)
    x, y = np.meshgrid(np.arange(cols), np.arange(rows), indexing="xy")
    pixels = np.zeros((MAX_PATCHES, found.shape[1]), np.float32)
    places = np.full((MAX_PATCHES, 2), -1, np.int64)
    pixels[:len(found)] = found
    places[:len(found)] = np.stack([x, y], axis=-1).reshape(-1, 2)
    return pixels, places, len(found) // (POOL * POOL)


def picture_ids(soft_tokens: int) -> list[int]:
    return [BOS, BOI] + [IMAGE] * soft_tokens + [EOI, EOS]


# ---- the model ------------------------------------------------------------------------------

# A Reshape attribute, as it's stored: name "allowzero", value 1. DirectML refuses Reshape with
# allowzero=1 ("The parameter is incorrect"), and here it changes nothing (no target size is
# zero), so the copy DirectML gets says 0, the default. Same length: nothing else moves.
_ALLOWZERO = b"\x0a\x09allowzero\x18\x01"


def for_directml(path: Path) -> Path:
    """The graph as DirectML takes it: a copy next to the original that uses the same weights file."""
    graph = path.read_bytes()
    if _ALLOWZERO not in graph:
        return path
    fixed = path.with_name(path.stem + "_directml.onnx")
    if not fixed.exists():
        tmp = fixed.with_suffix(".tmp")
        tmp.write_bytes(graph.replace(_ALLOWZERO, _ALLOWZERO[:-1] + b"\x00"))
        tmp.replace(fixed)
    return fixed


class OnnxModel:
    """Takes the place of the SentenceTransformer in Embedder: preprocess(), then embed()."""

    prompts = {"SearchQuery": QUERY_PROMPT}
    # One picture at a time: its encoder attends over all 2,520 patches (about 150 MB per picture),
    # and DirectML sees the same shape every time.
    image_batch = 1
    # DirectML prepares its work anew for every new input shape (about 0.2 s): texts are padded to
    # steps of this many tokens, so only a few shapes ever come up. (Padding is masked out.)
    STEP = 64

    def __init__(self, gpu: bool, device_id: int = 0):
        import onnxruntime as ort
        from tokenizers import Tokenizer

        self.gpu = gpu
        self.folder = download(gpu)
        self.tokenizer = Tokenizer.from_file(str(self.folder / "tokenizer.json"))
        self.tokenizer.enable_truncation(MAX_TOKENS)
        self._options = ort.SessionOptions()
        self._options.log_severity_level = 3
        if gpu:  # what DirectML needs
            self._options.enable_mem_pattern = False
            self._options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            self._providers = [("DmlExecutionProvider", {"device_id": device_id}), "CPUExecutionProvider"]
        else:
            self._providers = ["CPUExecutionProvider"]
        self._flavour = "fp16" if gpu else "quantized"
        self.text = self._session("model")
        self._vision = None  # loaded with the first picture: searching needs only text

    def _session(self, part: str):
        import onnxruntime as ort

        path = self.folder / "onnx" / f"{part}_{self._flavour}.onnx"
        if self.gpu:
            path = for_directml(path)
        return ort.InferenceSession(str(path), self._options, providers=self._providers)

    @property
    def vision(self):
        if self._vision is None:
            self._vision = self._session("vision_encoder")
        return self._vision

    @staticmethod
    def _input_length(item) -> int:
        return len(item) if isinstance(item, str) else 0

    def preprocess(self, items: list, prompt: str | None = None) -> dict:
        """Token ids for texts; for pictures, their patches too."""
        if all(isinstance(x, str) for x in items):
            encoded = self.tokenizer.encode_batch([(prompt or "") + x for x in items])
            return {"ids": [e.ids for e in encoded]}
        prepared = [patches(x["image"]) for x in items]
        return {"ids": [picture_ids(n) for _, _, n in prepared],
                "pixels": np.stack([p for p, _, _ in prepared]),
                "places": np.stack([q for _, q, _ in prepared])}

    def embed(self, features: dict) -> np.ndarray:
        """Unit vectors, cut to EMBED_DIM (the model is trained to be cut like that)."""
        pictures = _NO_FEATURES
        if "pixels" in features:
            pictures = self.vision.run(["image_features"], {
                "pixel_values": features["pixels"], "pixel_position_ids": features["places"]})[0]
            pictures = pictures.astype(np.float32, copy=False)
        ids = features["ids"]
        longest = max(len(i) for i in ids)
        if self.gpu:
            longest = -(-longest // self.STEP) * self.STEP
        input_ids = np.full((len(ids), longest), PAD, np.int64)
        mask = np.zeros((len(ids), longest), np.int64)
        for row, i in enumerate(ids):
            input_ids[row, :len(i)] = i
            mask[row, :len(i)] = 1
        out = self.text.run(["sentence_embedding"], {
            "input_ids": input_ids, "attention_mask": mask, "image_features": pictures,
            "video_features": _NO_FEATURES, "audio_features": _NO_FEATURES})[0]
        out = out[:, :config.EMBED_DIM].astype(np.float32)
        return out / np.maximum(np.linalg.norm(out, axis=1, keepdims=True), 1e-12)


# ---- which graphics card ------------------------------------------------------------------


def graphics_cards() -> list[tuple[int, str, int, int]]:
    """Windows' graphics adapters through DXGI: (index, name, vendor id, dedicated video memory).
    Software adapters (like the Basic Render Driver) are left out."""
    if os.name != "nt":
        return []
    import ctypes
    import uuid
    from ctypes import wintypes

    class Desc(ctypes.Structure):
        _fields_ = [("Description", ctypes.c_wchar * 128), ("VendorId", wintypes.UINT),
                    ("DeviceId", wintypes.UINT), ("SubSysId", wintypes.UINT), ("Revision", wintypes.UINT),
                    ("DedicatedVideoMemory", ctypes.c_size_t), ("DedicatedSystemMemory", ctypes.c_size_t),
                    ("SharedSystemMemory", ctypes.c_size_t), ("AdapterLuid", ctypes.c_int64),
                    ("Flags", wintypes.UINT)]

    def method(obj, index, *argtypes):
        table = ctypes.cast(obj, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        return ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *argtypes)(table[index])

    cards = []
    try:
        iid = (ctypes.c_ubyte * 16).from_buffer_copy(uuid.UUID("770aae78-f26f-4dba-a829-253c83d1b387").bytes_le)
        factory = ctypes.c_void_p()
        if ctypes.windll.dxgi.CreateDXGIFactory1(ctypes.byref(iid), ctypes.byref(factory)) != 0:
            return []
        try:
            enum = method(factory, 12, wintypes.UINT, ctypes.POINTER(ctypes.c_void_p))  # EnumAdapters1
            for index in range(16):
                adapter = ctypes.c_void_p()
                if enum(factory, index, ctypes.byref(adapter)) != 0:
                    break
                desc = Desc()
                method(adapter, 10, ctypes.POINTER(Desc))(adapter, ctypes.byref(desc))  # GetDesc1
                method(adapter, 2)(adapter)  # Release
                if not desc.Flags & 0x2:  # DXGI_ADAPTER_FLAG_SOFTWARE
                    cards.append((index, desc.Description, desc.VendorId, desc.DedicatedVideoMemory))
        finally:
            method(factory, 2)(factory)  # Release
    except (OSError, AttributeError, ValueError):
        return []
    return cards


def best_card() -> tuple[int, str] | None:
    """(index, name) of the graphics card with the most video memory of its own: on a laptop
    the dedicated one rather than the one built into the processor."""
    cards = graphics_cards()
    if not cards:
        return None
    index, name, _, _ = max(cards, key=lambda c: c[3])
    return index, name
