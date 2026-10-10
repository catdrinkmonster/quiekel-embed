"""EmbeddingGemma 2 wrapper. One model instance shared by indexing and search.

The model normally lives on the GPU. It can move to the CPU to give video memory
back (while a fullscreen app runs, or when there is nothing to index); searches keep working
there. Batches shrink automatically when video memory is tight.
"""

import logging
import os
import threading
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from . import config
from .governor import set_background_priority

log = logging.getLogger(__name__)
_thread_state = threading.local()


class GpuBusy(Exception):
    """The GPU ran out of memory even for a single item. Try again later."""


def model_cached() -> bool:
    """Checks the Hugging Face cache on disk without importing huggingface_hub."""
    home = Path(os.environ.get("HF_HOME", Path.home() / ".cache" / "huggingface"))
    hub = Path(os.environ.get("HF_HUB_CACHE", home / "hub"))
    snapshots = hub / f"models--{config.MODEL_ID.replace('/', '--')}" / "snapshots"
    return any(snapshots.glob("*/config.json"))


def _is_oom(e: BaseException) -> bool:
    return type(e).__name__ == "OutOfMemoryError" or "out of memory" in str(e).lower()


def _drop(ahead):
    """Forget a batch prepared ahead: it never starts, or its result is thrown away."""
    if ahead:
        ahead[2].cancel()


class Embedder:
    def __init__(self):
        self._model = None
        # Serialises model use so a search only waits for the current small batch.
        self._lock = threading.Lock()
        self.status = "not loaded"  # not loaded | downloading | loading | ready | asleep | error
        self.error: str | None = None
        self.cuda = False
        self.on_gpu = False
        self.gpu_name = ""
        self._gpu_dtypes: dict[str, object] = {}
        # Set by the indexer: called with the seconds each batch took, to pace itself.
        self.pace: Callable[[float], None] | None = None
        # Set by the indexer: < 1 shrinks batches when video memory is tight.
        self.batch_scale = 1.0
        # Set by the indexer: the helper thread that prepares batches runs at low priority too.
        self.background = False
        self._prep_lock = threading.Lock()
        self._pool: ThreadPoolExecutor | None = None
        self._gpu_prep = True  # resize images on the GPU while the model is there
        self._load_lock = threading.Lock()  # a search and indexing may both want to wake it
        self.last_used = time.monotonic()

    @property
    def ready(self) -> bool:
        return self._model is not None

    @property
    def device(self) -> str:
        if not self.ready:
            return ""
        if self.on_gpu:
            return self.gpu_name
        return "CPU (GPU memory released)" if self.cuda else "CPU"

    def load(self, prefer_gpu: bool = True):
        """Load the model; onto the CPU when prefer_gpu is False (e.g. a game is running)."""
        with self._load_lock:
            self._load(prefer_gpu)
        self.last_used = time.monotonic()

    def _load(self, prefer_gpu: bool):
        if self._model is not None:
            return
        self.error = None
        cached = model_cached()
        self.status = "loading" if cached else "downloading"
        if cached:
            # Skip ~a dozen update checks against huggingface.co, and work offline.
            # Must be set before huggingface_hub is first imported.
            os.environ.setdefault("HF_HUB_OFFLINE", "1")
        try:
            import torch
            from sentence_transformers import SentenceTransformer

            self.cuda = torch.cuda.is_available()
            gpu = _gpu_info() if self.cuda else None
            dtype = torch.bfloat16 if gpu and gpu[1] else torch.float32
            on_gpu = self.cuda and prefer_gpu
            model = SentenceTransformer(
                config.MODEL_ID,
                device="cuda" if on_gpu else "cpu",
                truncate_dim=config.EMBED_DIM,
                model_kwargs={"dtype": dtype},
                # Text + images (440M). Audio encoder isn't needed for files.
                config_kwargs={"audio_config": None},
            )
            model.eval()
            self._model = model
            if self.cuda:
                self.gpu_name = gpu[0] if gpu else "GPU"
                # Remember the GPU dtypes so moving back and forth is exact.
                self._gpu_dtypes = {n: t.dtype for n, t in _named_tensors(model)}
                if not on_gpu:
                    self._move(lambda name, t: ("cpu", torch.float32 if t.is_floating_point() else t.dtype))
            self.on_gpu = on_gpu
            self.status = "ready"
            log.info("Model ready on %s", self.device)
        except Exception as e:
            self.status = "error"
            self.error = f"{type(e).__name__}: {e}"
            log.exception("Model failed to load")
            raise

    def unload(self, why: str):
        """Let go of the model entirely (the "lean" memory setting, when it isn't used): its
        video memory and RAM are freed. The next search or file to index loads it again."""
        if not self.ready:
            return
        import gc

        with self._load_lock, self._lock:
            self._model = None
            self.on_gpu = False
            self.status = "asleep"
        gc.collect()
        self.free_cache()
        log.info("Model unloaded (%s)", why)

    # ---- GPU <-> CPU ------------------------------------------------------

    def release_gpu(self, why: str):
        """Move the model to the CPU and hand its video memory back to other apps."""
        if not (self.ready and self.on_gpu):
            return
        import torch

        with self._lock:
            started = time.perf_counter()
            # float32 on the CPU: bfloat16 math is slow on most CPUs. Upcasting is exact.
            self._move(lambda name, t: ("cpu", torch.float32 if t.is_floating_point() else t.dtype))
            self.on_gpu = False
            torch.cuda.empty_cache()
        log.info("Released GPU memory (%s) in %.1fs", why, time.perf_counter() - started)

    def use_gpu(self) -> bool:
        """Move the model back to the GPU. False if there isn't enough video memory."""
        if not self.ready or self.on_gpu or not self.cuda:
            return self.on_gpu
        import torch

        with self._lock:
            started = time.perf_counter()
            try:
                # Back to the exact dtypes it was loaded with, tensor by tensor (no fp32 peak).
                self._move(lambda name, t: ("cuda", self._gpu_dtypes.get(name, t.dtype)))
            except Exception as e:
                if not _is_oom(e):
                    raise
                self._move(lambda name, t: ("cpu", torch.float32 if t.is_floating_point() else t.dtype))
                torch.cuda.empty_cache()
                log.info("Not enough GPU memory to move the model back yet")
                return False
            self.on_gpu = True
        log.info("Model back on the GPU in %.1fs", time.perf_counter() - started)
        return True

    def _move(self, target):
        for module_name, module in self._model.named_modules():
            for kind in ("_parameters", "_buffers"):
                store = getattr(module, kind)
                for name, t in list(store.items()):
                    if t is None:
                        continue
                    full = f"{module_name}.{name}" if module_name else name
                    device, dtype = target(full, t)
                    if t.device.type != device or t.dtype != dtype:
                        if kind == "_parameters":
                            t.data = t.data.to(device=device, dtype=dtype)
                        else:
                            store[name] = t.to(device=device, dtype=dtype)

    def free_cache(self):
        if self.cuda:
            import torch

            torch.cuda.empty_cache()

    # ---- encoding -----------------------------------------------------------
    #
    # sentence-transformers' encode() does everything in turn: prepare a batch on the CPU
    # (tokenize, resize images), run it on the GPU, wait for the result. Done by hand here,
    # the next batch is prepared on a helper thread while the GPU runs the current one, and
    # images are resized on the GPU itself, so the GPU rarely waits for the CPU.

    def _prep(self, items: list, prompt: str | None):
        """Model-ready features for one batch."""
        import torch

        kwargs = {}
        if self.on_gpu and self._gpu_prep and any(isinstance(x, dict) and "image" in x for x in items):
            kwargs["processing_kwargs"] = {"image": {"device": "cuda"}}
        # Tokenizers aren't thread-safe; this lock is separate from the model's, so a search
        # never waits for a GPU batch just to tokenize its query.
        with self._prep_lock, torch.inference_mode():
            if not kwargs:
                return self._model.preprocess(items, prompt=prompt)
            try:
                return self._model.preprocess(items, prompt=prompt, **kwargs)
            except Exception as e:
                gpu_error = e
            features = self._model.preprocess(items, prompt=prompt)  # the CPU, as a fallback
            if _is_oom(gpu_error):
                self.free_cache()
            else:  # not supported here: stay on the CPU from now on
                self._gpu_prep = False
                log.info("Preparing images on the CPU (%s: %s)", type(gpu_error).__name__, gpu_error)
            return features

    def _prep_ahead(self, items: list, prompt: str | None):
        """_prep on the helper thread, at the same CPU priority as the indexer asking for it."""
        if getattr(_thread_state, "background", None) != self.background:
            set_background_priority(self.background)
            _thread_state.background = self.background
        return self._prep(items, prompt)

    def _forward(self, features) -> np.ndarray:
        import torch
        from sentence_transformers.util import batch_to_device, truncate_embeddings

        with self._lock, torch.inference_mode():
            model = self._model
            out = model(batch_to_device(features, model.device))["sentence_embedding"]
            out = truncate_embeddings(out, model.truncate_dim)
            return torch.nn.functional.normalize(out.float(), p=2, dim=1).cpu().numpy()

    def _embed(self, items: list, base: int, prompt: str | None = None) -> np.ndarray:
        """Embed in batches, longest first so each batch needs little padding; the next batch is
        prepared while the GPU works on this one. Batches shrink if video memory runs out."""
        out = np.zeros((len(items), config.EMBED_DIM), np.float32)
        if not items:
            return out
        length = getattr(self._model, "_input_length", lambda x: len(x) if isinstance(x, str) else 0)
        order = sorted(range(len(items)), key=lambda i: -length(items[i]))
        size = max(1, int(base * self.batch_scale))
        i, ahead = 0, None  # ahead: (start, size, future) of the batch being prepared
        if self._pool is None:
            self._pool = ThreadPoolExecutor(1, thread_name_prefix="embed-prep")
        try:
            while i < len(order):
                end = min(i + size, len(order))
                if ahead and ahead[:2] == (i, size):
                    features = ahead[2].result()
                else:
                    _drop(ahead)
                    features = self._prep([items[j] for j in order[i:end]], prompt)
                ahead = None
                if end < len(order):
                    upcoming = [items[j] for j in order[end:end + size]]
                    ahead = (end, size, self._pool.submit(self._prep_ahead, upcoming, prompt))
                started = time.perf_counter()
                try:
                    out[order[i:end]] = self._forward(features)
                except Exception as e:
                    if not _is_oom(e):
                        raise
                    features = None
                    _drop(ahead)
                    ahead = None
                    self.free_cache()
                    if size == 1:
                        raise GpuBusy("GPU out of memory") from e
                    size = max(1, size // 2)
                    log.info("GPU memory tight: batch size now %d", size)
                    continue
                i = end
                if self.pace:
                    self.pace(time.perf_counter() - started)
        finally:
            _drop(ahead)
        return out

    def documents(self, texts: list[str]) -> np.ndarray:
        """Embed passages already formatted as 'title: ... | text: ...'."""
        self.last_used = time.monotonic()
        # prompt="" stops sentence-transformers adding a default prompt on top.
        return self._embed(texts, config.TEXT_BATCH, prompt="")

    def images(self, images: list) -> np.ndarray:
        self.last_used = time.monotonic()
        return self._embed([{"image": im} for im in images], config.IMAGE_BATCH)

    def query(self, text: str) -> np.ndarray:
        self.last_used = time.monotonic()
        prompt = self._model.prompts.get("SearchQuery")
        return self._forward(self._prep([text], prompt))[0]


def _gpu_info() -> tuple[str, bool] | None:
    """(name, supports bfloat16) via NVML, which doesn't allocate video memory like CUDA does."""
    try:
        import pynvml

        pynvml.nvmlInit()
        handle = pynvml.nvmlDeviceGetHandleByIndex(0)
        major, _ = pynvml.nvmlDeviceGetCudaComputeCapability(handle)
        return pynvml.nvmlDeviceGetName(handle), major >= 8
    except Exception:
        try:
            import torch

            return torch.cuda.get_device_name(0), torch.cuda.is_bf16_supported()
        except Exception:
            return None


def _named_tensors(model):
    for module_name, module in model.named_modules():
        for kind in ("_parameters", "_buffers"):
            for name, t in getattr(module, kind).items():
                if t is not None:
                    yield (f"{module_name}.{name}" if module_name else name), t
